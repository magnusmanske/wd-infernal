use crate::wikidata::Wikidata;
use axum::http::StatusCode;
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};
use tokio::sync::OnceCell;
use tools_interface::{PetScan, Tool};
use url::Url;
use wikibase_rest_api::prelude::*;
use wikimisc::entity_id::{self, ItemId};
use wikimisc::mediawiki::Api;
use wikimisc::site_matrix::SiteMatrix;
use wikimisc::wikidata::WikidataError;

/// Loaded on first use. A failed load is retried by the next request, rather
/// than poisoning the cell for the rest of the process lifetime.
static SITE_MATRIX: OnceCell<SiteMatrix> = OnceCell::const_new();
/// The Wikidata API (which fetches the site info when it is created), shared
/// by all requests. Loaded and retried like [`SITE_MATRIX`].
static WIKIDATA_API: OnceCell<Api> = OnceCell::const_new();

static REST_API: LazyLock<Arc<RestApi>> =
    LazyLock::new(|| Arc::new(RestApi::wikidata().expect("Could not create RestApi")));

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ItemInfo {
    count: usize,
    local_page: Option<String>,
    already_in_category: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct CrossCats;

impl CrossCats {
    pub async fn cross_cats(
        category_item_id: &str,
        depth: u32,
        target_language: &str,
    ) -> Result<HashMap<String, ItemInfo>, StatusCode> {
        let category_item = Self::get_category_item(category_item_id).await?;
        Self::validate_category_item(&category_item)?;

        // Get the sites for the category
        let category_pages = category_item.sitelinks().sitelinks();

        // Get the items in the categories of the sites, via PetScan
        let target_wiki = format!("{target_language}wiki");
        let mut target_language_index = None;
        let mut futures = Vec::new();
        for category_sitelink in category_pages {
            if category_sitelink.wiki() == target_wiki {
                target_language_index = Some(futures.len());
            }
            futures.push(Self::items_in_local_category(category_sitelink, depth));
        }
        let results = join_all(futures).await;

        // Extract and deduplicate items from results
        let items: Vec<String> = results
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .flatten()
            .cloned()
            .collect();

        let mut item_info = HashMap::new();
        for item in items.iter() {
            item_info
                .entry(item.to_owned())
                .or_insert_with(|| ItemInfo {
                    count: 0,
                    local_page: None,
                    already_in_category: false,
                })
                .count += 1;
        }

        Self::remove_local_page_already_in_category(target_language_index, results, &mut item_info);
        Self::get_local_pages(&target_wiki, &mut item_info).await?;

        // Remove non-local items, and those already in the category
        item_info.retain(|_, v| v.local_page.is_some());
        item_info.retain(|_, v| !v.already_in_category);

        Ok(item_info)
    }

    fn validate_category_item(category_item: &Item) -> Result<(), StatusCode> {
        // Check if the item represents a category
        match category_item
            .statements()
            .property("P31")
            .iter()
            .filter_map(|statement| match statement.value() {
                StatementValue::Value(StatementValueContent::String(s)) => Some(s),
                _ => None,
            })
            .find(|s| *s == "Q4167836")
        {
            Some(_) => Ok(()),
            None => Err(StatusCode::NOT_FOUND),
        }
    }

    async fn get_category_item(category_item_id: &str) -> Result<Item, StatusCode> {
        let category_item_id: ItemId = category_item_id
            .parse()
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        let category_item_id = EntityId::Item(category_item_id.to_string());
        let category_item = Item::get(&category_item_id, &REST_API)
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?;
        Ok(category_item)
    }

    async fn wikidata_api() -> Result<&'static Api, WikidataError> {
        WIKIDATA_API
            .get_or_try_init(|| Wikidata::client().api())
            .await
    }

    async fn load_site_matrix() -> Result<SiteMatrix, StatusCode> {
        let api = Self::wikidata_api()
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        SiteMatrix::new(api)
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
    }

    async fn get_language_project_for_wiki(wiki: &str) -> Result<(String, String), StatusCode> {
        let url = SITE_MATRIX
            .get_or_try_init(Self::load_site_matrix)
            .await?
            .get_server_url_for_wiki(wiki)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let parsed_url = Url::parse(&url).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let host = parsed_url
            .host_str()
            .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
        let host_parts: Vec<&str> = host.split('.').collect();
        if host_parts.len() < 2 {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        Ok((host_parts[0].to_string(), host_parts[1].to_string()))
    }

    async fn items_in_local_category(
        category_sitelink: &Sitelink,
        depth: u32,
    ) -> Result<Vec<String>, StatusCode> {
        let category_page = category_sitelink
            .title()
            .split(':')
            .nth(1)
            .unwrap_or("")
            .to_string();
        let wiki = category_sitelink.wiki();
        let (language, project) = Self::get_language_project_for_wiki(wiki).await?;
        let mut petscan = PetScan::new(33506467);
        petscan
            .parameters_mut()
            .push(("language".to_string(), language));
        petscan
            .parameters_mut()
            .push(("project".to_string(), project));
        petscan
            .parameters_mut()
            .push(("categories".to_string(), category_page));
        petscan
            .parameters_mut()
            .push(("depth".to_string(), format!("{depth}")));
        petscan
            .run()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let items = petscan
            .pages()
            .iter()
            .filter(|page| page.page_namespace == 0) // only main namespace
            .map(|page| page.metadata.wikidata.to_owned())
            .collect::<Vec<String>>();
        Ok(items)
    }

    /// Sets `local_page` for items with a non-disambiguation page on `target_wiki`.
    async fn get_local_pages(
        target_wiki: &str,
        item_info: &mut HashMap<String, ItemInfo>,
    ) -> Result<(), StatusCode> {
        // Items already in the target category are dropped anyway
        let candidates: Vec<entity_id::EntityId> = item_info
            .iter()
            .filter(|(_, info)| !info.already_in_category)
            .filter_map(|(qid, _)| qid.parse::<ItemId>().ok())
            .map(entity_id::EntityId::from)
            .collect();
        let entities = Self::get_entities(
            &candidates,
            &[("props", "sitelinks"), ("sitefilter", target_wiki)],
        )
        .await?;
        let local_pages: HashMap<entity_id::EntityId, &str> = entities
            .iter()
            .filter_map(|(id, entity)| {
                Some((*id, entity["sitelinks"][target_wiki]["title"].as_str()?))
            })
            .collect();

        // Full claims are large, so only fetch them for items with a local page
        let with_local_page: Vec<entity_id::EntityId> = local_pages.keys().copied().collect();
        let claims = Self::get_entities(&with_local_page, &[("props", "claims")]).await?;

        for (id, title) in local_pages {
            if claims.get(&id).is_some_and(Self::is_disambiguation) {
                continue;
            }
            // `ItemId` parses the canonical form only, so this is the key.
            if let Some(info) = item_info.get_mut(&id.to_string()) {
                info.local_page = Some(title.to_string());
            }
        }
        Ok(())
    }

    /// Fetches entities via the shared Wikidata client (`wbgetentities`, 50
    /// IDs per request, a few requests at a time). Missing entities are left
    /// out; any failed request fails the whole call.
    async fn get_entities(
        ids: &[entity_id::EntityId],
        params: &[(&str, &str)],
    ) -> Result<HashMap<entity_id::EntityId, Value>, StatusCode> {
        Wikidata::client()
            .get_entities_with(ids, params)
            .await
            .map_err(|e| Self::upstream_error("wbgetentities", e))
    }

    fn is_disambiguation(entity: &Value) -> bool {
        entity["claims"]["P31"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|claim| claim["mainsnak"]["datavalue"]["value"]["id"] == "Q4167410")
    }

    fn upstream_error(source: &str, error: impl std::fmt::Display) -> StatusCode {
        tracing::error!("cross_cats: {source} failed: {error}");
        StatusCode::BAD_GATEWAY
    }

    fn remove_local_page_already_in_category(
        target_language_index: Option<usize>,
        results: Vec<Result<Vec<String>, StatusCode>>,
        item_info: &mut HashMap<String, ItemInfo>,
    ) {
        // Remove local items from the target language, if any
        if let Some(index) = target_language_index {
            if let Ok(target_results) = &results[index] {
                for q in target_results {
                    if let Some(info) = item_info.get_mut(q) {
                        info.already_in_category = true;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entity_with_p31(classes: &[&str]) -> Value {
        let claims: Vec<Value> = classes
            .iter()
            .map(|q| json!({"mainsnak": {"datavalue": {"value": {"id": q}}}}))
            .collect();
        json!({"claims": {"P31": claims}})
    }

    #[test]
    fn test_is_disambiguation() {
        assert!(CrossCats::is_disambiguation(&entity_with_p31(&[
            "Q4167410"
        ])));
        assert!(CrossCats::is_disambiguation(&entity_with_p31(&[
            "Q5", "Q4167410"
        ])));
        assert!(!CrossCats::is_disambiguation(&entity_with_p31(&["Q5"])));
        assert!(!CrossCats::is_disambiguation(&json!({"claims": {}})));
        assert!(
            !CrossCats::is_disambiguation(&Value::Null),
            "missing entity"
        );
    }

    #[test]
    fn test_remove_local_page_already_in_category() {
        let mut item_info: HashMap<String, ItemInfo> = ["Q1", "Q2"]
            .map(|q| (q.to_string(), ItemInfo::default()))
            .into();
        let results = vec![Ok(vec!["Q1".to_string()]), Ok(vec!["Q2".to_string()])];
        CrossCats::remove_local_page_already_in_category(Some(1), results, &mut item_info);
        assert!(!item_info["Q1"].already_in_category);
        assert!(item_info["Q2"].already_in_category);
    }
}
