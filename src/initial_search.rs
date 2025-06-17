use crate::WIKI_POOL;
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use toolforge::pool::mysql_async::{from_row, prelude::*};

lazy_static! {
    static ref RE_INITIAL: Regex = Regex::new(r"\b([A-Z])\b\.? *").unwrap();
}

pub struct InitialSearch {
    query: String,
}

impl InitialSearch {
    pub fn new(query: &str) -> Result<Self> {
        Ok(Self {
            query: query.to_string(),
        })
    }

    pub async fn run(&self) -> Result<Vec<String>> {
        const BASE_SQL: &str = r"SELECT DISTINCT concat('Q',`wbit_item_id`) AS `item`
	    	FROM `wbt_item_terms`,`wbt_term_in_lang`,`wbt_text_in_lang`
	     	WHERE `wbit_term_in_lang_id`=`wbtl_id`
	      	AND `wbtl_text_in_lang_id`=`wbxl_id`
	       	AND `wbxl_text_id` IN (SELECT `wbx_id` FROM `wbt_text`
	       		WHERE `wbx_text` LIKE :q1
	         	AND `wbx_text` RLIKE :q2
	        )";
        let q1 = RE_INITIAL.replace(&self.query, "$1%_").to_string(); // 'A%_A%_Saveliev'
        let q2 = RE_INITIAL.replace(&self.query, "$1.*? "); // '^A.*? A.*? Saveliev$'
        let q2 = format!("^{q2}$");
        let params = params! {
            "q1" => q1,
            "q2" => q2,
        };
        let mut conn = WIKI_POOL.connect("wikidatawiki").await?;
        let results = conn
            .exec_iter(BASE_SQL, params)
            .await?
            .map_and_drop(from_row::<String>)
            .await?;
        drop(conn);
        Ok(results)
    }
}
