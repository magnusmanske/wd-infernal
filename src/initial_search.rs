use crate::TOOLFORGE_DB;
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use wikimisc::mysql_async::{Params, from_row, params, prelude::Queryable};

lazy_static! {
    static ref RE_INITIAL: Regex = Regex::new(r"\b([A-Z])\b\.? *").unwrap();
}

/// Searches for items with a label that matches a human name with initials.
#[derive(Debug, Copy, Clone)]
pub struct InitialSearch;

impl InitialSearch {
    pub async fn run(query: &str) -> Result<Vec<String>> {
        let query = query.trim();
        let candidate_items = Self::get_candidate_items_from_term_store(query).await?;
        let futures = candidate_items
            .chunks(5000)
            .map(Self::filter_chunk)
            .collect::<Vec<_>>();
        let results = futures::future::try_join_all(futures)
            .await?
            .into_iter()
            .flatten()
            .collect();
        Ok(results)
    }

    async fn filter_chunk(chunk: &[String]) -> Result<Vec<String>> {
        let placeholders: String = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"SELECT DISTINCT `page_title`
			    FROM page,pagelinks,linktarget
			    WHERE page_title IN ({placeholders})
				AND page_namespace=0
				AND pl_from=page_id
				AND pl_target_id=lt_id
				AND lt_title='Q5'"#
        );
        let mut conn = TOOLFORGE_DB.get_connection("wikidata").await?;
        let results = conn
            .exec_iter(sql, chunk.to_vec())
            .await?
            .map_and_drop(from_row::<String>)
            .await?;
        drop(conn);
        Ok(results)
    }

    async fn get_candidate_items_from_term_store(query: &str) -> Result<Vec<String>> {
        const BASE_SQL: &str = r#"SELECT DISTINCT concat('Q',`wbit_item_id`) AS `item`
	    	FROM `wbt_item_terms`,`wbt_term_in_lang`,`wbt_text_in_lang`
	     	WHERE `wbit_term_in_lang_id`=`wbtl_id`
	      	AND `wbtl_text_in_lang_id`=`wbxl_id`
	       	AND `wbxl_text_id` IN (SELECT `wbx_id` FROM `wbt_text`
	       		WHERE `wbx_text` LIKE :q1
	         	AND `wbx_text` RLIKE :q2
	        )"#;
        let params = Self::generate_query_parameters(query);
        let mut conn = TOOLFORGE_DB.get_connection("termstore").await?;
        let results = conn
            .exec_iter(BASE_SQL, params)
            .await?
            .map_and_drop(from_row::<String>)
            .await?;
        drop(conn);
        Ok(results)
    }

    fn generate_query_parameters(query: &str) -> Params {
        let q1 = RE_INITIAL.replace_all(query, "$1%_").to_string(); // 'A%_A%_Saveliev'
        let q2 = RE_INITIAL.replace_all(query, "$1.*? "); // '^A.*? A.*? Saveliev$'
        let q2 = format!("^{q2}$");
        params! {
            "q1" => q1,
            "q2" => q2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_query_parameters() {
        let query = "A.A.Saveliev";
        let expected = params! {
            "q1" => "A%_A%_Saveliev",
            "q2" => "^A.*? A.*? Saveliev$",
        };
        let params = InitialSearch::generate_query_parameters(query);
        assert_eq!(params, expected);
    }

    #[tokio::test]
    async fn test_initial_search() {
        let query = "H.M.Manske";
        let results = InitialSearch::run(query).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], "Q13520818");
    }
}
