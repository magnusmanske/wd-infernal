use axum::http::StatusCode;
use serde_json::Value;
use std::sync::{LazyLock, OnceLock};
use wikimisc::entity_id::{ItemId, PropertyId};
use wikimisc::sparql_client::{SparqlClient, SparqlError};
use wikimisc::sparql_endpoint::SparqlEndpoint;
use wikimisc::sparql_util::values_clause;
use wikimisc::wikibase::Snak;
use wikimisc::wikidata::{CirrusQuery, Wikidata as WikidataClient};

/// What requests to Wikidata and WDQS say they are. Wikimedia asks for
/// contact information.
const USER_AGENT: &str = wikimisc::user_agent!();

/// Retries of a SPARQL query that failed in a way that may go away on its own.
/// One only: a request is waiting.
const SPARQL_RETRIES: u32 = 1;

/// How many search hits [`Wikidata::search_names_by_class`] looks at.
const NAME_SEARCH_LIMIT: usize = 10;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Wikidata;

impl Wikidata {
    pub fn infernal_reference_snak() -> Snak {
        Snak::new_item("P887", "Q131287902") // based on heuristic: Wikidata Infernal
    }

    /// The Wikidata Query Service. WDQS v1 (Blazegraph) for now: v2
    /// (`SparqlEndpoint::wikidata_main`) refuses user agents it has not
    /// allow-listed until its public launch. Queries are plain SPARQL, except
    /// where `Location::p131` has to try one form per engine.
    pub fn sparql_client() -> Result<&'static SparqlClient, StatusCode> {
        static CLIENT: OnceLock<SparqlClient> = OnceLock::new();
        if let Some(client) = CLIENT.get() {
            return Ok(client);
        }
        let client = SparqlClient::new(SparqlEndpoint::wdqs(), USER_AGENT)
            .map_err(|e| Self::sparql_failure(&e))?
            .with_retries(SPARQL_RETRIES);
        Ok(CLIENT.get_or_init(|| client))
    }

    /// Runs a SPARQL query and returns the JSON result.
    pub async fn sparql_json(sparql: &str) -> Result<Value, StatusCode> {
        Self::sparql_client()?
            .query_json(sparql)
            .await
            .map_err(|e| Self::sparql_failure(&e))
    }

    /// Logs a SPARQL failure; the caller's request fails with a 500.
    pub fn sparql_failure(error: &SparqlError) -> StatusCode {
        tracing::warn!(%error, "SPARQL query failed");
        StatusCode::INTERNAL_SERVER_ERROR
    }

    /// The Wikidata API client shared by all requests.
    pub fn client() -> &'static WikidataClient {
        static CLIENT: LazyLock<WikidataClient> = LazyLock::new(|| WikidataClient::new(USER_AGENT));
        &CLIENT
    }

    /// Logs a Wikidata API failure; the caller's request fails with a 500.
    fn api_failure(error: &wikimisc::wikidata::WikidataError) -> StatusCode {
        tracing::warn!(%error, "Wikidata API request failed");
        StatusCode::INTERNAL_SERVER_ERROR
    }

    /// Searches Wikidata via the API for items with the exact label `name` that
    /// are an instance of (P31) the class `p31`. Returns *all* matching items.
    pub async fn search_names_by_class(name: &str, p31: &str) -> Result<Vec<ItemId>, StatusCode> {
        // It goes into the search and the SPARQL query.
        let p31: ItemId = p31.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
        // A quoted phrase, so the name cannot inject search keywords like `insource:`.
        let phrase = name.replace('"', " ");
        let query =
            CirrusQuery::new(&format!("\"{phrase}\"")).has_statement(PropertyId::new(31), p31);
        let results = Self::client()
            .search_items(&query.to_string(), NAME_SEARCH_LIMIT)
            .await
            .map_err(|e| Self::api_failure(&e))?;
        if results.is_empty() {
            return Ok(results);
        }
        let values = values_clause("?q", results);

        // Labels are compared in `items_with_label`, so the name never becomes
        // part of the query.
        let sparql = format!(
            r#"SELECT DISTINCT ?q ?label {{
          {values}
          ?q wdt:P31 wd:{p31} ; rdfs:label ?label
          }}"#
        );

        let json = Self::sparql_json(&sparql).await?;
        Ok(Self::items_with_label(&json, name))
    }

    /// The items `?q` in a SPARQL result whose `?label` is exactly `label`.
    fn items_with_label(json: &Value, label: &str) -> Vec<ItemId> {
        let mut items: Vec<ItemId> = json["results"]["bindings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|binding| binding["label"]["value"].as_str() == Some(label))
            .filter_map(|binding| ItemId::from_uri(binding["q"]["value"].as_str()?).ok())
            .collect();
        items.sort();
        items.dedup();
        items
    }

    /// Like [`Self::search_names_by_class`] but returns an item only when there
    /// is exactly one match. Used where a unique item is required (family names).
    pub async fn search_single_name(name: &str, p31: &str) -> Result<Vec<ItemId>, StatusCode> {
        let mut items = Self::search_names_by_class(name, p31).await?;
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
    use wikimisc::wikibase::{EntityType, EntityValue};

    #[tokio::test]
    #[ignore]
    async fn test_search_single_name() {
        let results = Wikidata::search_single_name("Manske", "Q101352")
            .await
            .unwrap();
        assert_eq!(results, vec![ItemId::new(1891133)]);
    }

    /// Untrusted item IDs are parsed into an `ItemId` before they go into a
    /// SPARQL query or a URL path; only the canonical form gets through.
    #[test]
    fn test_item_id_gate() {
        for id in ["Q1", "Q42", "Q131287902"] {
            assert!(id.parse::<ItemId>().is_ok(), "{id} should be valid");
        }
        for id in [
            "",
            "Q",
            "Q0",
            "Q01",
            "q42",
            "P31",
            "Q42 ",
            "Q4-2",
            "Q42}",
            "Q1/../x",
            "Q1 } ?x ?y ?z {",
        ] {
            assert!(id.parse::<ItemId>().is_err(), "{id} should be invalid");
        }
    }

    #[tokio::test]
    async fn test_search_names_by_class_rejects_invalid_class() {
        for p31 in ["", "P31", "Q1 } ?x ?y ?z {"] {
            assert_eq!(
                Wikidata::search_names_by_class("Manske", p31).await,
                Err(StatusCode::BAD_REQUEST)
            );
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
        assert_eq!(
            Wikidata::items_with_label(&json, "Manske"),
            vec![ItemId::new(1)]
        );
        assert!(Wikidata::items_with_label(&json, "Nobody").is_empty());
        assert!(Wikidata::items_with_label(&serde_json::json!({}), "Manske").is_empty());
    }

    #[tokio::test]
    async fn test_wd_infernal_reference() {
        let snak = Wikidata::infernal_reference_snak();
        assert_eq!(
            snak.data_value().as_ref().unwrap().value().to_owned(),
            wikimisc::wikibase::Value::Entity(EntityValue::new(EntityType::Item, "Q131287902"))
        );
        assert_eq!(snak.property(), "P887");
    }
}
