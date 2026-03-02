use crate::TOOLFORGE_DB;
use anyhow::Result;
use std::collections::HashMap;
use wikimisc::mysql_async::{from_row, prelude::Queryable};

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
            let item_ids: Vec<String> = chunk.iter().map(|t| t[1..].to_string()).collect();
            let placeholders: String = std::iter::repeat_n("?", item_ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT concat('Q',ips_item_id),ips_site_page FROM wb_items_per_site WHERE ips_site_id=? AND ips_item_id IN ({placeholders})"
            );
            // Prepend wiki_to as the first positional parameter
            let mut params: Vec<String> = Vec::with_capacity(item_ids.len() + 1);
            params.push(wiki_to.to_string());
            params.extend(item_ids);
            let results = conn
                .exec_iter(sql, params)
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
            let titles: Vec<String> = chunk.iter().map(|t| t.replace('_', " ")).collect();
            let placeholders: String = std::iter::repeat_n("?", titles.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT ips_site_page,concat('Q',ips_item_id) FROM wb_items_per_site WHERE ips_site_id=? AND ips_site_page IN ({placeholders})"
            );
            // Prepend wiki_from as the first positional parameter
            let mut params: Vec<String> = Vec::with_capacity(titles.len() + 1);
            params.push(wiki_from.to_string());
            params.extend(titles);
            let results = conn
                .exec_iter(sql, params)
                .await?
                .map_and_drop(from_row::<(String, String)>)
                .await?;
            ret.extend(results);
        }
        drop(conn);
        Ok(ret)
    }

    /// Normalize a wiki name to a safe lowercase string of only ASCII letters and underscores.
    fn normalize_wiki(wiki: &str) -> String {
        wiki.trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_lowercase() || *c == '_')
            .collect::<String>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn check_db_connection() -> bool {
        TOOLFORGE_DB.get_connection("termstore").await.is_ok()
    }

    #[tokio::test]
    async fn test_wd2site() {
        if !check_db_connection().await {
            // No DB connection
            return;
        }
        let change_wiki = ChangeWiki::new("wikidatawiki", vec!["Q13520818".to_string()]);
        let result = change_wiki.wd2site("enwiki").await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("Q13520818").unwrap(), "Magnus Manske");
    }

    #[tokio::test]
    async fn test_site2wd() {
        if !check_db_connection().await {
            // No DB connection
            return;
        }
        let change_wiki = ChangeWiki::new("enwiki", vec!["Magnus_Manske".to_string()]);
        let result = change_wiki.site2wd().await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("Magnus Manske").unwrap(), "Q13520818");
    }

    // ── normalize_wiki ────────────────────────────────────────────────────────

    #[test]
    fn test_normalize_wiki_already_lowercase() {
        assert_eq!(ChangeWiki::normalize_wiki("enwiki"), "enwiki");
    }

    #[test]
    fn test_normalize_wiki_uppercase_is_lowered() {
        assert_eq!(ChangeWiki::normalize_wiki("EnWiki"), "enwiki");
    }

    #[test]
    fn test_normalize_wiki_all_caps() {
        assert_eq!(ChangeWiki::normalize_wiki("DEWIKI"), "dewiki");
    }

    #[test]
    fn test_normalize_wiki_trims_surrounding_whitespace() {
        assert_eq!(ChangeWiki::normalize_wiki("  enwiki  "), "enwiki");
    }

    #[test]
    fn test_normalize_wiki_strips_digits() {
        // Digits are not in [a-z] or '_', so they are filtered out
        assert_eq!(ChangeWiki::normalize_wiki("wiki123"), "wiki");
    }

    #[test]
    fn test_normalize_wiki_strips_hyphens_and_dots() {
        assert_eq!(ChangeWiki::normalize_wiki("en-wiki.org"), "enwikiorg");
    }

    #[test]
    fn test_normalize_wiki_preserves_underscores() {
        assert_eq!(ChangeWiki::normalize_wiki("wikidata_wiki"), "wikidata_wiki");
    }

    #[test]
    fn test_normalize_wiki_empty_string() {
        assert_eq!(ChangeWiki::normalize_wiki(""), "");
    }

    #[test]
    fn test_normalize_wiki_only_special_chars_returns_empty() {
        assert_eq!(ChangeWiki::normalize_wiki("123!@#$%"), "");
    }

    #[test]
    fn test_normalize_wiki_mixed_case_with_extras() {
        // Uppercase letters, digits, and punctuation are all stripped/lowered
        assert_eq!(ChangeWiki::normalize_wiki("  En_Wiki2! "), "en_wiki");
    }

    // ── convert: same-wiki short-circuit (no DB required) ────────────────────

    #[tokio::test]
    async fn test_convert_same_wiki_returns_identity_map() {
        // When from and to normalise to the same wiki, each title maps to itself
        let titles = vec![
            "Douglas Adams".to_string(),
            "The Hitchhiker's Guide".to_string(),
        ];
        let cw = ChangeWiki::new("enwiki", titles.clone());
        let result = cw.convert("enwiki").await.unwrap();
        assert_eq!(result.len(), titles.len());
        for title in &titles {
            assert_eq!(
                result.get(title).unwrap(),
                title,
                "same-wiki convert should map each title to itself"
            );
        }
    }

    #[tokio::test]
    async fn test_convert_same_wiki_after_normalization() {
        // "EnWiki" and "enwiki" normalise to the same string, so no DB is hit
        let titles = vec!["Some Page".to_string()];
        let cw = ChangeWiki::new("enwiki", titles.clone());
        let result = cw.convert("EnWiki").await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("Some Page").unwrap(), "Some Page");
    }

    #[tokio::test]
    async fn test_convert_same_wiki_empty_titles() {
        let cw = ChangeWiki::new("enwiki", vec![]);
        let result = cw.convert("enwiki").await.unwrap();
        assert!(
            result.is_empty(),
            "identity map of zero titles must be empty"
        );
    }

    #[tokio::test]
    async fn test_convert_same_wiki_single_title() {
        let cw = ChangeWiki::new("frwiki", vec!["Paris".to_string()]);
        let result = cw.convert("frwiki").await.unwrap();
        assert_eq!(result.get("Paris").unwrap(), "Paris");
    }
}
