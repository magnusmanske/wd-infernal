pub mod location;
pub mod person;
pub mod server;
pub mod wikidata;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().len() > 1 {
        // current development test
        let ret = location::Location::country_for_location_and_date("Q365", 1921)
            .await
            .unwrap();
        println!("{ret:?}");
        Ok(())
    } else {
        server::Server::start().await
    }
}
