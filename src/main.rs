pub mod location;
pub mod person;
pub mod server;
pub mod wikidata;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().len() > 1 {
        // current development test
        let ret = person::Person::name_gender("Heinrich Magnus Manske")
            .await
            .unwrap();
        println!("{ret:?}");
        Ok(())
    } else {
        server::Server::start().await
    }
}
