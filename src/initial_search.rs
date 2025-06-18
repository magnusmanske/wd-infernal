use crate::TOOLFORGE_DB;
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use wikimisc::mysql_async::{from_row, params, prelude::Queryable, Params};

lazy_static! {
    static ref RE_INITIAL: Regex = Regex::new(r"\b([A-Z])\b\.? *").unwrap();
}

/// Searches for items with a label that matches a human name with initials.
pub struct InitialSearch {}

impl InitialSearch {
    pub async fn run(query: &str) -> Result<Vec<String>> {
        const BASE_SQL: &str = r"SELECT DISTINCT concat('Q',`wbit_item_id`) AS `item`
	    	FROM `wbt_item_terms`,`wbt_term_in_lang`,`wbt_text_in_lang`
	     	WHERE `wbit_term_in_lang_id`=`wbtl_id`
	      	AND `wbtl_text_in_lang_id`=`wbxl_id`
	       	AND `wbxl_text_id` IN (SELECT `wbx_id` FROM `wbt_text`
	       		WHERE `wbx_text` LIKE :q1
	         	AND `wbx_text` RLIKE :q2
	        )
			# is a human
			HAVING (SELECT count(*) FROM page,pagelinks,linktarget
			WHERE page_title=item AND page_namespace=0
			AND pl_from=page_id
			AND pl_target_id=lt_id
			AND lt_title='Q5')>0";
        println!("{BASE_SQL}");
        let params = Self::generate_query_parameters(query);
        let mut conn = TOOLFORGE_DB.get_connection("wikidata").await?; //.connect("wikidatawiki").await?;
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
        println!("{q1}\n{q2}");
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
