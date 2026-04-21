use async_lazy::Lazy;
use axum::http::StatusCode;
use futures::StreamExt;
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};
use tools_interface::{PetScan, Tool};
use url::Url;
use wikibase::mediawiki::api::Api;
use wikibase_rest_api::prelude::*;
use wikimisc::site_matrix::SiteMatrix;

static SITE_MATRIX: Lazy<SiteMatrix> = Lazy::new(|| {
    Box::pin(async {
        CrossCats::load_site_matrix()
            .await
            .expect("Could not load site matrix")
    })
});

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
        Self::get_local_pages(target_wiki, items, &mut item_info).await?;

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
        let category_item_id = EntityId::Item(category_item_id.to_string());
        let category_item = Item::get(category_item_id, &REST_API)
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?;
        Ok(category_item)
    }

    async fn load_site_matrix() -> Result<SiteMatrix, StatusCode> {
        let api = Api::new("https://www.wikidata.org/w/api.php")
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        SiteMatrix::new(&api)
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
    }

    async fn get_language_project_for_wiki(wiki: &str) -> Result<(String, String), StatusCode> {
        let url = SITE_MATRIX
            .force()
            .await
            .get_server_url_for_wiki(wiki)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let parsed_url = Url::parse(&url).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let host = parsed_url
            .host_str()
            .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
        let host_parts = host.split('.').collect::<Vec<&str>>();
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

    async fn get_local_pages(
        target_wiki: String,
        items: Vec<String>,
        item_info: &mut HashMap<String, ItemInfo>,
    ) -> Result<(), StatusCode> {
        let item_entity_ids: Vec<EntityId> = items
            .into_iter()
            .filter_map(|id| EntityId::new(id).ok())
            .filter(|eid| matches!(eid, EntityId::Item(_)))
            .collect();
        let fetches = item_entity_ids.into_iter().map(|eid| {
            let api = REST_API.clone();
            async move { Item::get(eid, &api).await }
        });
        let loaded_items: Vec<Item> = futures::stream::iter(fetches)
            .buffer_unordered(5)
            .filter_map(|res: Result<Item, RestApiError>| async move { res.ok() })
            .collect()
            .await;

        for item in &loaded_items {
            let q = match item.id().id() {
                Ok(id) => id.to_string(),
                Err(_) => continue,
            };
            let is_disambiguation = item.statements().property("P31").iter().any(|statement| {
                matches!(statement.value(), StatementValue::Value(StatementValueContent::String(s)) if s == "Q4167410")
            });
            if is_disambiguation {
                continue;
            }
            if let Some(sitelink) = item.sitelinks().get_wiki(&target_wiki) {
                if let Some(info) = item_info.get_mut(&q) {
                    info.local_page = Some(sitelink.title().to_string());
                }
            }
        }
        Ok(())
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
