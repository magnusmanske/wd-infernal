use std::net::SocketAddr;

use axum::{
    extract::Path,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use mediawiki::Api;
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    trace::TraceLayer,
    // trace::TraceLayer,
};
use wikibase::{Snak, Statement};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let cors = CorsLayer::new().allow_origin(Any);

    let app = Router::new()
        .route("/", get(root))
        .route("/P131/:latitude/:longitude", get(p131))
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new())
        .layer(cors);

    let port: u16 = match std::env::var("WD_INFERNAL_PORT") {
        Ok(port) => port.as_str().parse::<u16>().unwrap_or(8000),
        Err(_) => 8000,
    };

    let address = [0, 0, 0, 0]; // TODOO env::var("AC2WD_ADDRESS")

    let addr = SocketAddr::from((address, port));
    tracing::debug!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Could not create listener");
    axum::serve(listener, app)
        .await
        .expect("Could not start server");
    Ok(())
}

async fn root() -> impl IntoResponse {
    let ret = include_str!("../static/root.html");
    Html(ret)
}

fn _wd_infernal_qualifier() -> Snak {
    Snak::new_item("P887", "") // based on heuristic: Wikidata Infernal
}

async fn p131(
    Path((latitude, longitude)): Path<(f64, f64)>,
) -> Result<impl IntoResponse, StatusCode> {
    // TODO try list=geosearch?
    let radius_km = 1;
    let sparql = format!(
        r#"SELECT ?p131 {{
	        ?q wdt:P625 ?loc ; wdt:P131 ?p131 .

	        SERVICE wikibase:around {{
	          ?q wdt:P625 ?coords .
	          bd:serviceParam wikibase:center "Point({longitude} {latitude})"^^geo:wktLiteral .
	          bd:serviceParam wikibase:radius "{radius_km}" .
	          bd:serviceParam wikibase:distance ?distance
	        }}

	      SERVICE wikibase:label {{
	        bd:serviceParam wikibase:language "en" .
	      }}
	    }}
	    ORDER BY DESC(?distance)
	    LIMIT 5"#
    );
    let api = match Api::new("https://www.wikidata.org/w/api.php").await {
        Ok(api) => api,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let json = match api.sparql_query(&sparql).await {
        Ok(json) => json,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let mut entities = api.entities_from_sparql_result(&json, "p131");
    entities.sort();
    entities.dedup();
    let statements: Vec<_> = entities
        .iter()
        .map(|entity| {
            let snak = Snak::new_item("P131", entity);
            let qualifiers = vec![
                // wd_infernal_qualifier(),
                Snak::new_item("P3452", "Q96623327"), // inferred from coordinate location
            ];
            Statement::new_normal(snak, qualifiers, vec![])
        })
        .collect();
    Ok(Json(statements))
}
