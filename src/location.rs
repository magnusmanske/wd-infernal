use crate::wikidata::Wikidata;
use axum::http::StatusCode;
use wikibase::{Reference, Snak, Statement};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Location;

impl Location {
    pub async fn country_for_location_and_date(
        place_q: &str,
        year: i32,
    ) -> Result<Vec<Statement>, StatusCode> {
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
        let api = Wikidata::get_wikidata_api().await?;
        let json = match api.sparql_query(&sparql).await {
            Ok(json) => json,
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        };
        let bindings = match json["results"]["bindings"].as_array() {
            Some(b) => b,
            None => return Ok(vec![]),
        };
        let mut no_years = None;
        let mut both_years = None;
        let mut one_year = None;
        for b in bindings {
            let country = match b["country"]["value"].as_str() {
                Some(c) => c,
                None => continue,
            };
            let country = match api.extract_entity_from_uri(country) {
                Ok(c) => c,
                Err(_) => continue,
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
            let snak = Snak::new_item("P17", &country);
            let reference = Reference::new(vec![
                Wikidata::infernal_reference_snak(),
                Snak::new_item("P3452", "Q131293105"), // inferred from place and date
            ]);
            let statement = Statement::new_normal(snak, vec![], vec![reference]);
            statements.push(statement);
        }
        Ok(statements)
    }

    pub async fn p131(latitude: f64, longitude: f64) -> Result<Vec<Statement>, StatusCode> {
        // TODO try list=geosearch?
        let radius_km = 1;
        let sparql = format!(
            r#"SELECT ?p131 {{
		        ?q wdt:P625 ?loc ; wdt:P131 ?p131 .

		        SERVICE wikibase:around {{
		          ?q wdt:P625 ?coords .
		          bd:serviceParam wikibase:center "Point({longitude} {latitude})"^^geo:wktLiteral .
		          bd:serviceParam wikibase:radius "{radius_km}" .
		          bd:serviceParam wikibase:distance ?distance
		        }}

		      SERVICE wikibase:label {{
		        bd:serviceParam wikibase:language "en" .
		      }}
		    }}
		    ORDER BY DESC(?distance)
		    LIMIT 5"#
        );
        let api = Wikidata::get_wikidata_api().await?;
        let json = match api.sparql_query(&sparql).await {
            Ok(json) => json,
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        };
        let mut entities = api.entities_from_sparql_result(&json, "p131");
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use wikibase::{EntityType, EntityValue};

    #[tokio::test]
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
