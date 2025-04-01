pub mod crosscats;
pub mod location;
pub mod person;
pub mod referee;
pub mod server;
pub mod wikidata;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().len() > 1 {
        // current development test
        let ret = crosscats::CrossCats::cross_cats("Q9649201", 1, "en")
            .await
            .unwrap();
        println!("{ret:#?}");
        Ok(())
    } else {
        server::Server::start().await
    }
}
