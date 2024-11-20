use axum::http::StatusCode;
use wikibase::{Reference, Snak, Statement};

use crate::wikidata::Wikidata;

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Location {}

impl Location {
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
}
