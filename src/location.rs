use crate::wikidata::{Wikidata, is_item_id, item_from_uri};
use axum::http::StatusCode;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use wikibase::{Reference, Snak, Statement};
use wikimisc::sparql_client::SparqlError;

/// Radius around a coordinate within which items lend their P131.
const P131_RADIUS_KM: f64 = 1.0;
/// Kilometres per degree of latitude (mean Earth radius × π / 180).
const KM_PER_DEGREE: f64 = 111.195;

/// Whether the endpoint has answered the portable form of the nearby query,
/// so it is tried first; see [`Location::nearby_p131_json`].
static PORTABLE_NEARBY_WORKS: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Location;

impl Location {
    pub async fn country_for_location_and_date(
        place_q: &str,
        year: i32,
    ) -> Result<Vec<Statement>, StatusCode> {
        if !is_item_id(place_q) {
            return Err(StatusCode::BAD_REQUEST);
        }
        // get preferred and normal country statements, but not deprecated ones
        let sparql = format!(
            r#"SELECT ?country ?year_from ?year_to {{
	      wd:{place_q} p:P17 ?c .
	      ?c ps:P17 ?country .
	      ?c wikibase:rank ?rank . FILTER(?rank != wikibase:DeprecatedRank) .
	      OPTIONAL {{ ?c pq:P580 ?date_from . BIND(year(?date_from) AS ?year_from) }}
	      OPTIONAL {{ ?c pq:P582 ?date_to . BIND(year(?date_to) AS ?year_to) }}
	      }}"#
        );
        let json = Wikidata::sparql_json(&sparql).await?;
        let bindings = match json["results"]["bindings"].as_array() {
            Some(b) => b,
            None => return Ok(vec![]),
        };
        let mut no_years = None;
        let mut both_years = None;
        let mut one_year = None;
        for b in bindings {
            let Some(country) = b["country"]["value"].as_str().and_then(item_from_uri) else {
                continue;
            };
            let year_from = b["year_from"]["value"]
                .as_str()
                .and_then(|y| y.parse::<i32>().ok());
            let year_to = b["year_to"]["value"]
                .as_str()
                .and_then(|y| y.parse::<i32>().ok());
            if year_from.is_none() && year_to.is_none() {
                no_years = Some(country);
            } else if let (Some(year_from), Some(year_to)) = (&year_from, &year_to) {
                if year >= *year_from && year <= *year_to {
                    both_years = Some(country);
                }
            } else if let (Some(year_from), None) = (year_from, year_to) {
                if year >= year_from {
                    one_year = Some(country);
                }
            } else if let Some(year_to) = &year_to {
                if year <= *year_to {
                    one_year = Some(country);
                }
            }
        }
        let mut statements = vec![];
        if let Some(country) = both_years.or(one_year).or(no_years) {
            let snak = Snak::new_item("P17", country);
            let reference = Reference::new(vec![
                Wikidata::infernal_reference_snak(),
                Snak::new_item("P3452", "Q131293105"), // inferred from place and date
            ]);
            let statement = Statement::new_normal(snak, vec![], vec![reference]);
            statements.push(statement);
        }
        Ok(statements)
    }

    /// The P131 (located in) of the items nearest to a coordinate, up to five
    /// items within [`P131_RADIUS_KM`].
    pub async fn p131(latitude: f64, longitude: f64) -> Result<Vec<Statement>, StatusCode> {
        // TODO try list=geosearch?
        if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
            return Err(StatusCode::BAD_REQUEST);
        }
        let json = Self::nearby_p131_json(latitude, longitude).await?;
        let mut entities: Vec<&str> = json["results"]["bindings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|binding| item_from_uri(binding["p131"]["value"].as_str()?))
            .collect();
        entities.sort();
        entities.dedup();
        let statements: Vec<_> = entities
            .iter()
            .map(|entity| {
                let snak = Snak::new_item("P131", entity);
                let reference = Reference::new(vec![
                    Wikidata::infernal_reference_snak(),
                    Snak::new_item("P3452", "Q96623327"), // inferred from coordinate location
                ]);
                Statement::new_normal(snak, vec![], vec![reference])
            })
            .collect();
        Ok(statements)
    }

    /// Runs the nearby-P131 query in whichever form the endpoint supports.
    ///
    /// No form works well on both engines. `Blazegraph` (WDQS v1) has
    /// `wikibase:around` and no spatial index, so the portable form times out
    /// there; `QLever` (WDQS v2) rejects `wikibase:around` at once (HTTP 400).
    /// The form that worked last is tried first, so a switch of engine — even
    /// behind an unchanged URL — costs one rejected request.
    async fn nearby_p131_json(latitude: f64, longitude: f64) -> Result<Value, StatusCode> {
        let client = Wikidata::sparql_client()?;
        let around = Self::around_p131_query(latitude, longitude);
        let portable = Self::portable_p131_query(latitude, longitude);
        let mut forms = [(false, around), (true, portable)];
        if PORTABLE_NEARBY_WORKS.load(Ordering::Relaxed) {
            forms.reverse();
        }
        let mut last_error = None;
        for (is_portable, query) in forms {
            match client.query_json(&query).await {
                Ok(json) => {
                    PORTABLE_NEARBY_WORKS.store(is_portable, Ordering::Relaxed);
                    return Ok(json);
                }
                Err(error @ SparqlError::Endpoint { status: 400, .. }) => last_error = Some(error),
                Err(error) => return Err(Wikidata::sparql_failure(&error)),
            }
        }
        Err(
            last_error.map_or(StatusCode::INTERNAL_SERVER_ERROR, |error| {
                Wikidata::sparql_failure(&error)
            }),
        )
    }

    /// The nearby query for Blazegraph's `wikibase:around` service.
    fn around_p131_query(latitude: f64, longitude: f64) -> String {
        format!(
            r#"SELECT ?p131 ?distance {{
  SERVICE wikibase:around {{
    ?q wdt:P625 ?loc .
    bd:serviceParam wikibase:center "Point({longitude} {latitude})"^^geo:wktLiteral .
    bd:serviceParam wikibase:radius "{P131_RADIUS_KM}" .
    bd:serviceParam wikibase:distance ?distance .
  }}
  ?q wdt:P131 ?p131 .
}}
ORDER BY ASC(?distance)
LIMIT 5"#
        )
    }

    /// The nearby query in plain SPARQL 1.1: a bounding box on the
    /// coordinate's value node, then the distance on a flat approximation of
    /// the Earth, which is accurate to well under a percent at this radius. A
    /// box crossing the antimeridian is not wrapped around.
    fn portable_p131_query(latitude: f64, longitude: f64) -> String {
        let delta_lat = P131_RADIUS_KM / KM_PER_DEGREE;
        // Longitude degrees shrink towards the poles; capped, so a pole does
        // not make the box infinitely wide.
        let lon_scale = latitude.to_radians().cos().max(0.01);
        let delta_lon = delta_lat / lon_scale;
        let (south, north) = (latitude - delta_lat, latitude + delta_lat);
        let (west, east) = (longitude - delta_lon, longitude + delta_lon);
        let km_per_lon_degree = KM_PER_DEGREE * lon_scale;
        let max_squared = P131_RADIUS_KM * P131_RADIUS_KM;
        format!(
            r#"SELECT ?p131 ?squared {{
  ?q p:P625/psv:P625 ?v .
  ?v wikibase:geoLatitude ?lat ; wikibase:geoLongitude ?lon ; wikibase:geoGlobe wd:Q2 .
  FILTER(?lat >= {south} && ?lat <= {north} && ?lon >= {west} && ?lon <= {east})
  BIND(((?lat - ({latitude})) * {KM_PER_DEGREE}) * ((?lat - ({latitude})) * {KM_PER_DEGREE})
     + ((?lon - ({longitude})) * {km_per_lon_degree}) * ((?lon - ({longitude})) * {km_per_lon_degree}) AS ?squared)
  FILTER(?squared <= {max_squared})
  ?q wdt:P131 ?p131 .
}}
ORDER BY ASC(?squared)
LIMIT 5"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wikibase::{EntityType, EntityValue};

    #[tokio::test]
    async fn test_country_for_location_and_date_rejects_bad_item() {
        let result = Location::country_for_location_and_date("Q1 } ?x ?y ?z {", 2000).await;
        assert_eq!(result.unwrap_err(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_p131_rejects_impossible_coordinates() {
        for (lat, lon) in [
            (91.0, 0.0),
            (0.0, -181.0),
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
        ] {
            assert_eq!(
                Location::p131(lat, lon).await.unwrap_err(),
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[test]
    fn test_nearby_queries_parse_and_order_nearest_first() {
        for query in [
            Location::around_p131_query(52.19, -0.13),
            Location::portable_p131_query(-33.86, 151.2),
        ] {
            assert!(query.contains("ORDER BY ASC("), "{query}");
            assert!(!query.contains("wikibase:label"), "{query}");
        }
        let portable = Location::portable_p131_query(52.19, -0.13);
        assert!(!portable.contains("SERVICE"), "{portable}");
        assert!(portable.contains("?lon <= -0.115"), "{portable}");
    }

    #[tokio::test]
    #[ignore]
    async fn test_p131() {
        let latitude = 52.19422713089248;
        let longitude = 0.13009437319916947;
        let result = Location::p131(latitude, longitude).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0]
                .main_snak()
                .data_value()
                .as_ref()
                .unwrap()
                .value()
                .to_owned(),
            wikibase::Value::Entity(EntityValue::new(EntityType::Item, "Q21713103"))
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_country_for_location_and_date() {
        let statements = Location::country_for_location_and_date("Q365", 1921)
            .await
            .unwrap();
        assert_eq!(statements.len(), 1);
        let statement = &statements[0];
        let value = statement.main_snak().data_value().as_ref().unwrap().value();
        assert_eq!(
            *value,
            wikibase::Value::Entity(EntityValue::new(EntityType::Item, "Q41304"))
        );
    }
}
