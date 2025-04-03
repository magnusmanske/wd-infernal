pub mod crosscats;
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
            "referee" => {
                let item = std::env::args().nth(2).unwrap();
                let ret = referee::Referee::new()
                    .await
                    .unwrap()
                    .get_potential_references(&item)
                    .await
                    .unwrap();
                // println!("{ret:#?}");
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
