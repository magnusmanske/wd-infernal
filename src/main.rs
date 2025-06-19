use lazy_static::lazy_static;
use serde_json::json;
use std::fs::File;
use wikibase_rest_api::Patch as _;
use wikimisc::toolforge_db::ToolforgeDB;

pub mod crosscats;
pub mod initial_search;
pub mod isbn;
pub mod location;
pub mod person;
pub mod referee;
pub mod server;
pub mod viaf;
pub mod wikidata;

lazy_static! {
    pub static ref TOOLFORGE_DB: ToolforgeDB = {
        /* For local testing:
        ssh magnus@login.toolforge.org -L 3309:wikidatawiki.web.db.svc.eqiad.wmflabs:3306 -N &
         */
        let file = match File::open("config.json") {
            Ok(file) => file,
            Err(_) => File::open("/data/project/wd-infernal/wd-infernal/config.json").expect("Unable to open config file"),
        };
        let reader = std::io::BufReader::new(file);
        let config: serde_json::Value = serde_json::from_reader(reader).unwrap();
        let mut ret = ToolforgeDB::default();
        ret.add_mysql_pool("wikidata",&config["wikidata"]).unwrap();
        ret
    };
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().len() > 2 {
        let command = std::env::args().nth(1).unwrap();
        match command.as_str() {
            "viaf" => {
                let query = std::env::args().nth(2).unwrap();
                let result = viaf::search_viaf_for_local_names(&query).await.unwrap();
                println!("{result:#?}");
            }
            "isbn" => {
                let item_id = std::env::args().nth(2).unwrap();
                let mut isbn2wiki = isbn::ISBN2wiki::new_from_item(&item_id).await.unwrap();
                isbn2wiki.retrieve().await.unwrap();
                let patch = isbn2wiki.generate_patch(&item_id).unwrap();
                println!("{}", json!(patch.patch()));
            }
            "referee" => {
                let item = std::env::args().nth(2).unwrap();
                let ret = referee::Referee::new()
                    .await
                    .unwrap()
                    .get_potential_references(&item)
                    .await
                    .unwrap();
                println!("{ret:#?}");
                println!("{}", ret.len());
            }
            "crosscats" => {
                let item = std::env::args().nth(2).unwrap();
                let depth: u32 = std::env::args()
                    .nth(2)
                    .unwrap_or_else(|| "0".to_string())
                    .parse()
                    .unwrap();
                let language = std::env::args().nth(2).unwrap_or_else(|| "en".to_string());
                let ret = crosscats::CrossCats::cross_cats(&item, depth, &language)
                    .await
                    .unwrap();
                println!("{ret:#?}");
            }
            "initial_search" => {
                let query = std::env::args().nth(2).unwrap();
                let ret = initial_search::InitialSearch::run(&query).await.unwrap();
                println!("{ret:#?}");
            }
            other => {
                println!("{other} not implemented in main")
            }
        }
        Ok(())
    } else {
        server::Server::start().await
    }
}
