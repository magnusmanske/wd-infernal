use serde_json::json;
use wikibase_rest_api::Patch as _;

pub mod crosscats;
pub mod isbn;
pub mod location;
pub mod person;
pub mod referee;
pub mod server;
pub mod wikidata;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().len() > 2 {
        let command = std::env::args().nth(1).unwrap();
        match command.as_str() {
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
            other => {
                println!("{other} not implemented in main")
            }
        }
        Ok(())
    } else {
        server::Server::start().await
    }
}
