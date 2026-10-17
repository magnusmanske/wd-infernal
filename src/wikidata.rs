use axum::http::StatusCode;
use mediawiki::{Api, hashmap};
use serde_json::Value;
use std::collections::HashMap;
use wikibase::Snak;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Wikidata;

/// Whether `id` is a well-formed item ID such as `Q42`. Untrusted IDs must
/// pass this before they go into a SPARQL query or a URL path.
pub fn is_item_id(id: &str) -> bool {
    id.strip_prefix('Q').is_some_and(|num| {
        num.starts_with(|c: char| ('1'..='9').contains(&c))
            && num.bytes().all(|b| b.is_ascii_digit())
    })
}

impl Wikidata {
    pub fn infernal_reference_snak() -> Snak {
        Snak::new_item("P887", "Q131287902") // based on heuristic: Wikidata Infernal
    }

    pub async fn get_wikidata_api() -> Result<Api, StatusCode> {
        Api::new("https://www.wikidata.org/w/api.php")
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// Perform a Wikidata API search and return matching item titles.
    async fn api_search(api: &Api, query: &str) -> Result<Vec<String>, StatusCode> {
        let params: HashMap<String, String> =
            hashmap!["action"=>"query","list"=>"search","srnamespace"=>"0","srsearch"=>query]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        let results = api
            .get_query_api_json(&params)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let results = match results["query"]["search"].as_array() {
            Some(v) => v,
            None => return Ok(vec![]),
        };
        Ok(results
            .iter()
            .filter_map(|result| result["title"].as_str().map(|s| s.to_string()))
            .collect())
    }

    pub async fn search_items(api: &Api, query: &str) -> Result<Vec<String>, StatusCode> {
        Self::api_search(api, query).await
    }

    /// Searches Wikidata via the API for items with the exact label `name` that
    /// are an instance of (P31) the class `p31`. Returns *all* matching items.
    pub async fn search_names_by_class(
        api: &Api,
        name: &str,
        p31: &str,
    ) -> Result<Vec<String>, StatusCode> {
        // A quoted phrase, so the name cannot inject search keywords like `insource:`.
        let phrase = name.replace('"', " ");
        let query = format!("\"{phrase}\" haswbstatement:P31={p31}");
        let results: Vec<String> = Self::api_search(api, &query)
            .await?
            .into_iter()
            .filter(|q| is_item_id(q))
            .collect();
        if results.is_empty() {
            return Ok(results);
        }
        let values = results.join(" wd:");

        // Labels are compared in `items_with_label`, so the name never becomes
        // part of the query.
        let sparql = format!(
            r#"SELECT DISTINCT ?q ?label {{
          VALUES ?q {{ wd:{values} }}
          ?q wdt:P31 wd:{p31} ; rdfs:label ?label
          }}"#
        );

        let json = api
            .sparql_query(&sparql)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(Self::items_with_label(&json, name))
    }

    /// The items `?q` in a SPARQL result whose `?label` is exactly `label`.
    fn items_with_label(json: &Value, label: &str) -> Vec<String> {
        let mut items: Vec<String> = json["results"]["bindings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|binding| binding["label"]["value"].as_str() == Some(label))
            .filter_map(|binding| binding["q"]["value"].as_str()?.rsplit('/').next())
            .filter(|q| is_item_id(q))
            .map(str::to_string)
            .collect();
        items.sort();
        items.dedup();
        items
    }

    /// Like [`Self::search_names_by_class`] but returns an item only when there
    /// is exactly one match. Used where a unique item is required (family names).
    pub async fn search_single_name(
        api: &Api,
        name: &str,
        p31: &str,
    ) -> Result<Vec<String>, StatusCode> {
        let mut items = Self::search_names_by_class(api, name, p31).await?;
        // If there are multiple items, the match is ambiguous: return none.
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
    #[ignore]
    async fn test_search_single_name() {
        let api = Api::new("https://www.wikidata.org/w/api.php")
            .await
            .unwrap();
        let results = Wikidata::search_single_name(&api, "Manske", "Q101352")
            .await
            .unwrap();
        assert_eq!(results, vec!["Q1891133"]);
    }

    #[test]
    fn test_is_item_id() {
        for id in ["Q1", "Q42", "Q131287902"] {
            assert!(is_item_id(id), "{id} should be valid");
        }
        for id in [
            "", "Q", "Q0", "Q01", "q42", "P31", "Q42 ", "Q4-2", "Q42}", "Q1/../x",
            "Q1 } ?x ?y ?z {",
        ] {
            assert!(!is_item_id(id), "{id} should be invalid");
        }
    }

    #[test]
    fn test_items_with_label_matches_exactly() {
        let json = serde_json::json!({"results": {"bindings": [
            {"q": {"value": "http://www.wikidata.org/entity/Q1"}, "label": {"value": "Manske"}},
            {"q": {"value": "http://www.wikidata.org/entity/Q1"}, "label": {"value": "Manske"}},
            {"q": {"value": "http://www.wikidata.org/entity/Q2"}, "label": {"value": "manske"}},
            {"q": {"value": "http://www.wikidata.org/entity/Q3"}, "label": {"value": "Manske\" || true"}},
            {"q": {"value": "http://www.wikidata.org/entity/Q4}"}, "label": {"value": "Manske"}},
        ]}});
        assert_eq!(Wikidata::items_with_label(&json, "Manske"), vec!["Q1"]);
        assert!(Wikidata::items_with_label(&json, "Nobody").is_empty());
        assert!(Wikidata::items_with_label(&serde_json::json!({}), "Manske").is_empty());
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
