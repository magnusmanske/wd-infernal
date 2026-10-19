use crate::database;
use anyhow::Result;
use regex::Regex;
use std::sync::LazyLock;
use wikimisc::mysql_async::{Params, from_row, params, prelude::Queryable};

static RE_INITIAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([A-Z])\b\.? *").unwrap());

/// Searches for items with a label that matches a human name with initials.
#[derive(Debug, Copy, Clone)]
pub struct InitialSearch;

impl InitialSearch {
    pub async fn run(query: &str) -> Result<Vec<String>> {
        let query = query.trim();
        let candidate_items = Self::get_candidate_items_from_term_store(query).await?;
        let futures = candidate_items
            .chunks(500)
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
        let mut conn = database::wikidata().await?;
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
        let mut conn = database::termstore().await?;
        let results = conn
            .exec_iter(BASE_SQL, params)
            .await?
            .map_and_drop(from_row::<String>)
            .await?;
        drop(conn);
        Ok(results)
    }

    /// The `LIKE` (`q1`) and `RLIKE` (`q2`) patterns for `query`, in which
    /// each initial stands for a name starting with it. The rest of the query
    /// is user input, and matched literally, so it can't widen the search
    /// (e.g. `%`) or make the database run an expensive regular expression.
    fn generate_query_parameters(query: &str) -> Params {
        let mut q1 = String::new(); // 'A%_A%_Saveliev'
        let mut q2 = String::new(); // '^A.*? A.*? Saveliev$'
        let mut rest = 0;
        for caps in RE_INITIAL.captures_iter(query) {
            let (initial, letter) = (caps.get(0).expect("whole match"), &caps[1]);
            let literal = &query[rest..initial.start()];
            q1 += &format!("{}{letter}%_", Self::escape_like(literal));
            q2 += &format!("{}{letter}.*? ", regex::escape(literal));
            rest = initial.end();
        }
        let literal = &query[rest..];
        q1 += &Self::escape_like(literal);
        let q2 = format!("^{q2}{}$", regex::escape(literal));
        params! {
            "q1" => q1,
            "q2" => q2,
        }
    }

    /// `text` as a `LIKE` pattern matching only itself.
    fn escape_like(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
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

    #[test]
    fn test_generate_query_parameters_escapes_user_input() {
        for (query, q1, q2) in [
            ("%", r"\%", "^%$"),
            (r"J. St_\(x)", r"J%_St\_\\(x)", r"^J.*? St_\\\(x\)$"),
        ] {
            let expected = params! { "q1" => q1, "q2" => q2 };
            assert_eq!(InitialSearch::generate_query_parameters(query), expected);
        }
    }

    #[test]
    #[ignore = "needs the databases, see run_live_tests.sh"]
    fn test_initial_search() {
        database::block_on(async {
            let query = "H.M.Manske";
            let results = InitialSearch::run(query).await.unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0], "Q13520818");
            // Only a literal "%", not every label
            assert!(InitialSearch::run("%").await.unwrap().is_empty());
        });
    }
}
