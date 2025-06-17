use crate::initial_search::InitialSearch;
use crate::isbn::ISBN2wiki;
use crate::person::Person;
use crate::referee::Referee;
use crate::{crosscats::CrossCats, location::Location};
use axum::{
    extract::Path,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::net::SocketAddr;
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use wikibase_rest_api::Patch;

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Server {}

impl Server {
    pub async fn start() -> Result<(), Box<dyn std::error::Error>> {
        tracing_subscriber::fmt::init();

        let cors = CorsLayer::new().allow_origin(Any);

        let app = Router::new()
            .route("/", get(Self::root))
            .route("/P131/:latitude/:longitude", get(Self::p131))
            .route("/name_gender/:name", get(Self::name_gender))
            .route("/country_year/:item/:year", get(Self::country_year))
            .route("/referee/:item", get(Self::referee))
            .route("/viaf_search/:query", get(Self::viaf_search))
            .route("/isbn/item/:item", get(Self::isbn_item))
            .route("/isbn/isbn/:isbn", get(Self::isbn_isbn))
            .route("/initial_search/:query", get(Self::initial_search))
            .route(
                "/cross_categories/:category_item/:language/:depth",
                get(Self::cross_cats),
            )
            .route(
                "/country_year/:item/:year/:property",
                get(Self::country_year_property),
            )
            .layer(TraceLayer::new_for_http())
            .layer(CompressionLayer::new())
            .layer(cors);

        let addr = Self::get_server_address();
        tracing::debug!("listening on {addr}");
        println!("listening on http://{addr}");
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("Could not create listener");
        axum::serve(listener, app)
            .await
            .expect("Could not start server");
        Ok(())
    }

    fn get_server_address() -> SocketAddr {
        let port: u16 = match std::env::var("WD_INFERNAL_PORT") {
            Ok(port) => port.as_str().parse::<u16>().unwrap_or(8000),
            Err(_) => 8000,
        };

        let address = [0, 0, 0, 0];
        // TODOO env::var("WD_INFERNAL_ADDRESS")

        SocketAddr::from((address, port))
    }

    async fn root() -> impl IntoResponse {
        let ret = include_str!("../static/root.html");
        Html(ret)
    }

    async fn initial_search(Path(query): Path<String>) -> Result<impl IntoResponse, StatusCode> {
        let is = InitialSearch::new(&query).unwrap();
        let ret = is.run().await.unwrap();
        Ok(Json(ret))
    }

    async fn name_gender(Path(name): Path<String>) -> Result<impl IntoResponse, StatusCode> {
        let statements = Person::name_gender(&name).await?;
        Ok(Json(statements))
    }

    async fn p131(
        Path((latitude, longitude)): Path<(f64, f64)>,
    ) -> Result<impl IntoResponse, StatusCode> {
        let statements = Location::p131(latitude, longitude).await?;
        Ok(Json(statements))
    }

    async fn cross_cats(
        Path((category_item, language, depth)): Path<(String, String, u32)>,
    ) -> Result<impl IntoResponse, StatusCode> {
        let results = CrossCats::cross_cats(&category_item, depth, &language).await?;
        Ok(Json(results))
    }

    async fn isbn_isbn(Path(isbn): Path<String>) -> Result<impl IntoResponse, StatusCode> {
        let mut isbn2wiki = ISBN2wiki::new(&isbn).ok_or(StatusCode::NOT_FOUND)?;
        isbn2wiki
            .retrieve()
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?;
        let ret = isbn2wiki
            .generate_item()
            .map_err(|_| StatusCode::NOT_FOUND)?;
        let ret = json!({"item": ret});
        Ok(Json(ret))
    }

    async fn isbn_item(Path(item): Path<String>) -> Result<impl IntoResponse, StatusCode> {
        let mut isbn2wiki = ISBN2wiki::new_from_item(&item).await.unwrap();
        isbn2wiki.retrieve().await.unwrap();
        let patch = isbn2wiki.generate_patch(&item).unwrap();
        let ret = patch.patch().to_owned();
        Ok(Json(ret))
    }

    async fn viaf_search(Path(query): Path<String>) -> Result<impl IntoResponse, StatusCode> {
        let results = crate::viaf::search_viaf_for_local_names(&query)
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?;
        Ok(Json(results))
    }

    async fn referee(Path(item): Path<String>) -> Result<impl IntoResponse, StatusCode> {
        let results = Referee::new()
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?
            .get_potential_references(&item)
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?;
        Ok(Json(results))
    }

    async fn country_year(
        Path((item, year)): Path<(String, i32)>,
    ) -> Result<impl IntoResponse, StatusCode> {
        let statements = Location::country_for_location_and_date(&item, year).await?;
        Ok(Json(statements))
    }

    async fn country_year_property(
        Path((item, year, property)): Path<(String, i32, String)>,
    ) -> Result<impl IntoResponse, StatusCode> {
        let mut statements = Location::country_for_location_and_date(&item, year).await?;
        for statement in &mut statements {
            statement.set_property(&property.to_uppercase());
        }
        Ok(Json(statements))
    }
}
