use axum::http::StatusCode;
use mediawiki::{hashmap, Api};
use std::collections::HashMap;
use wikibase::Snak;

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Wikidata {}

impl Wikidata {
    pub fn infernal_reference_snak() -> Snak {
        Snak::new_item("P887", "Q131287902") // based on heuristic: Wikidata Infernal
    }

    pub async fn get_wikidata_api() -> Result<Api, StatusCode> {
        match Api::new("https://www.wikidata.org/w/api.php").await {
            Ok(api) => Ok(api),
            Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    }

    pub async fn search_items(api: &Api, query: &str) -> Result<Vec<String>, StatusCode> {
        let params: HashMap<String, String> =
            hashmap!["action"=>"query","list"=>"search","srnamespace"=>"0","srsearch"=>&query]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        let results = match api.get_query_api_json(&params).await {
            Ok(v) => v,
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        };
        let results = match results["query"]["search"].as_array() {
            Some(v) => v,
            None => return Ok(vec![]),
        };
        let results: Vec<String> = results
            .iter()
            .map(|result| result["title"].as_str().unwrap().to_owned())
            .collect();
        Ok(results)
    }

    // Searches Wikidata via the API
    pub async fn search_single_name(
        api: &Api,
        name: &str,
        p31: &str,
    ) -> Result<Vec<String>, StatusCode> {
        let query = format!("{name} haswbstatement:P31={p31}");
        let params: HashMap<String, String> =
            hashmap!["action"=>"query","list"=>"search","srnamespace"=>"0","srsearch"=>&query]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        let results = match api.get_query_api_json(&params).await {
            Ok(v) => v,
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        };
        let results = match results["query"]["search"].as_array() {
            Some(v) => v,
            None => return Ok(vec![]),
        };
        let results: Vec<String> = results
            .iter()
            .map(|result| result["title"].as_str().unwrap().to_owned())
            .collect();
        if results.is_empty() {
            return Ok(results);
        }
        let values = results.join(" wd:");

        let sparql = format!(
            r#"SELECT DISTINCT ?q {{
          VALUES ?q {{ wd:{values} }}
          ?q wdt:P31 wd:{p31} ; rdfs:label ?label . FILTER ( str(?label)="{name}" )
          }}"#
        );

        let json = match api.sparql_query(&sparql).await {
            Ok(json) => json,
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        };
        let mut items = api.entities_from_sparql_result(&json, "q");
        items.sort();
        items.dedup();

        // If there are multiple items, return none
        if items.len() > 1 {
            items.clear();
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wikibase::{EntityType, EntityValue};

    #[tokio::test]
    async fn test_search_single_name() {
        let api = Api::new("https://www.wikidata.org/w/api.php")
            .await
            .unwrap();
        let results = Wikidata::search_single_name(&api, "Manske", "Q101352")
            .await
            .unwrap();
        assert_eq!(results, vec!["Q1891133"]);
    }

    #[tokio::test]
    async fn test_wd_infernal_reference() {
        let snak = Wikidata::infernal_reference_snak();
        assert_eq!(
            snak.data_value().as_ref().unwrap().value().to_owned(),
            wikibase::Value::Entity(EntityValue::new(EntityType::Item, "Q131287902"))
        );
        assert_eq!(snak.property(), "P887");
    }
}
