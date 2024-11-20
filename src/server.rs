use crate::location::Location;
use crate::person::Person;
use axum::{
    extract::Path,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use std::net::SocketAddr;
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};

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
            .layer(TraceLayer::new_for_http())
            .layer(CompressionLayer::new())
            .layer(cors);

        let addr = Self::get_server_address();
        tracing::debug!("listening on {}", addr);
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
}
