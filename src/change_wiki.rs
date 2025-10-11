use anyhow::Result;
use std::collections::HashMap;
use wikimisc::mysql_async::{from_row, prelude::Queryable};

use crate::TOOLFORGE_DB;

#[derive(Debug)]
pub struct ChangeWiki {
    wiki_from: String,
    titles: Vec<String>,
}

impl ChangeWiki {
    pub fn new(wiki_from: &str, titles: Vec<String>) -> Self {
        ChangeWiki {
            wiki_from: Self::normalize_wiki(wiki_from),
            titles,
        }
    }

    pub async fn convert(&self, wiki_to: &str) -> Result<HashMap<String, String>> {
        let wiki_to = Self::normalize_wiki(wiki_to);
        if self.wiki_from == wiki_to {
            return Ok(self
                .titles
                .iter()
                .map(|ft| (ft.clone(), ft.clone()))
                .collect());
        }
        if self.wiki_from == "wikidatawiki" {
            self.wd2site(&wiki_to).await
        } else if wiki_to == "wikidatawiki" {
            self.site2wd().await
        } else {
            let site2wd = self.site2wd().await?;
            let items = site2wd.values().cloned().collect();
            let tmp = Self::new("wikidatawiki", items);
            let wd2site = tmp.wd2site(&wiki_to).await?;
            Ok(site2wd
                .iter()
                .filter_map(|(source_page, item)| {
                    let target_page = wd2site.get(item)?;
                    Some((source_page.to_owned(), target_page.to_owned()))
                })
                .collect())
        }
    }

    async fn wd2site(&self, wiki_to: &str) -> Result<HashMap<String, String>> {
        let mut conn = TOOLFORGE_DB.get_connection("wikidata").await?;
        let mut ret: HashMap<String, String> = HashMap::new();
        for chunk in self.titles.chunks(5000) {
            let chunk: Vec<String> = chunk.iter().map(|t| t[1..].to_string()).collect();
            let placeholders: String = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT concat('Q',ips_item_id),ips_site_page FROM wb_items_per_site WHERE ips_site_id='{wiki_to}' AND ips_item_id IN ({placeholders})"
            );
            let results = conn
                .exec_iter(sql, chunk)
                .await?
                .map_and_drop(from_row::<(String, String)>)
                .await?;
            ret.extend(results);
        }
        drop(conn);
        Ok(ret)
    }

    async fn site2wd(&self) -> Result<HashMap<String, String>> {
        let wiki_from = &self.wiki_from;
        let mut conn = TOOLFORGE_DB.get_connection("wikidata").await?;
        let mut ret: HashMap<String, String> = HashMap::new();
        for chunk in self.titles.chunks(5000) {
            let chunk: Vec<String> = chunk.iter().map(|t| t.replace('_', " ")).collect();
            let placeholders: String = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT ips_site_page,concat('Q',ips_item_id) FROM wb_items_per_site WHERE ips_site_id='{wiki_from}' AND ips_site_page IN ({placeholders})"
            );
            let results = conn
                .exec_iter(sql, chunk)
                .await?
                .map_and_drop(from_row::<(String, String)>)
                .await?;
            ret.extend(results);
        }
        drop(conn);
        Ok(ret)
    }

    /// Normalize a wiki name to a lowercase string with only digits and underscores.
    /// Result is safe for database use
    fn normalize_wiki(wiki: &str) -> String {
        wiki.trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| (*c >= 'a' && *c <= 'z') || *c == '_')
            .collect::<String>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_wd2site() {
        let change_wiki = ChangeWiki::new("wikidatawiki", vec!["Q13520818".to_string()]);
        let result = change_wiki.wd2site("enwiki").await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("Q13520818").unwrap(), "Magnus Manske");
    }

    #[tokio::test]
    async fn test_site2wd() {
        let change_wiki = ChangeWiki::new("enwiki", vec!["Magnus_Manske".to_string()]);
        let result = change_wiki.site2wd().await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("Magnus Manske").unwrap(), "Q13520818");
    }
}
