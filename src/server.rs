use crate::TOOLFORGE_DB;
use crate::initial_search::InitialSearch;
use crate::isbn::ISBN2wiki;
use crate::person::Person;
use crate::referee::Referee;
use crate::{crosscats::CrossCats, location::Location};
use axum::extract::Query;
use axum::http::header::{CACHE_CONTROL, HeaderValue};
use axum::middleware;
use axum::routing::post;
use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
};
use serde::Deserialize;
use serde_json::json;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tower_http::{
    compression::CompressionLayer,
    cors::{AllowOrigin, Any, CorsLayer},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;
use wikibase_rest_api::Patch;
use wikimisc::mysql_async::prelude::Queryable;

#[derive(Deserialize)]
struct Format {
    format: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Server;

/// A simple sliding-window rate limiter middleware.
/// Limits to `max_requests` per `window` duration, with burst = `max_requests`.
#[derive(Clone, Debug)]
struct RateLimiter {
    state: Arc<Mutex<VecDeque<Instant>>>,
    max_requests: usize,
    window: Duration,
}

impl RateLimiter {
    fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(VecDeque::new())),
            max_requests,
            window,
        }
    }

    async fn check(&self) {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        let cutoff = now - self.window;
        while state.front().is_some_and(|t| *t <= cutoff) {
            state.pop_front();
        }
        if state.len() >= self.max_requests {
            drop(state);
            // Wait until the oldest request expires
            tokio::time::sleep(self.window).await;
            Box::pin(self.check()).await;
        } else {
            state.push_back(now);
        }
    }
}

async fn rate_limit_middleware(
    axum::extract::State(limiter): axum::extract::State<RateLimiter>,
    req: axum::extract::Request,
    next: middleware::Next,
) -> impl IntoResponse {
    limiter.check().await;
    next.run(req).await
}

// ── Module-level helper ──────────────────────────────────────────────────────

fn items2table(items: &[String]) -> String {
    let mut html = items
        .iter()
        .enumerate()
        .map(|(num, q)| {
            format!(
                "<tr><th>{}</th><td><a q='{q}'>{q}</a></td><td><tt>{q}</tt></td><td class='desc' data-q='{q}'><div class='wd-desc'></div><div class='autodesc text-muted small font-italic'></div></td><td class='birth' data-q='{q}'></td><td class='death' data-q='{q}'></td></tr>",
                num + 1
            )
        })
        .collect::<Vec<String>>()
        .join("\n");
    html = format!(
        "<table class='table table-striped'><thead><th>#</th><th>Label</th><th>Item</th><th>Description</th><th>Born</th><th>Died</th></thead><tbody>{html}</tbody></table>"
    );
    html
}

// ── Route handlers ───────────────────────────────────────────────────────────

/// Health check endpoint that verifies database connectivity
#[utoipa::path(
    get,
    path = "/healthz",
    responses((status = 200, description = "Database connected"), (status = 503, description = "Database unavailable"))
)]
async fn healthz() -> impl IntoResponse {
    match tokio::time::timeout(Duration::from_secs(2), async {
        let mut conn = TOOLFORGE_DB.get_connection("wikidata").await?;
        let _: Option<u8> = conn.query_first("SELECT 1").await?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    {
        Ok(Ok(())) => StatusCode::OK,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// Root endpoint returning the main HTML page
#[utoipa::path(
    get,
    path = "/",
    responses((status = 200, description = "Main page HTML"))
)]
async fn root() -> impl IntoResponse {
    let ret = include_str!("../static/root.html");
    Html(ret)
}

/// Search for Wikidata items by name
#[utoipa::path(
    get,
    path = "/initial_search/{query}",
    params(("query" = String, Path, description = "Search query"), ("format" = Option<String>, Query, description = "Response format (html or json)")),
    responses((status = 200, description = "Search results"))
)]
async fn initial_search(
    Path(query): Path<String>,
    params: Query<Format>,
) -> Result<impl IntoResponse, StatusCode> {
    tracing::info!("initial_search: {query}");
    let ret = InitialSearch::run(&query).await.map_err(|e| {
        tracing::warn!("initial_search failed: {e}");
        StatusCode::BAD_REQUEST
    })?;
    match params.format.as_deref() {
        Some("html") => {
            let escaped_query = html_escape::encode_text(&query);
            let form = format!(
                "<form id='search-form' class='mb-3'>\
                    <div class='input-group'>\
                        <input type='text' id='search-input' class='form-control' value=\"{escaped_query}\" placeholder='Search name'>\
                        <div class='input-group-append'>\
                            <button type='submit' class='btn btn-primary'>Search</button>\
                        </div>\
                    </div>\
                </form>"
            );
            let body = if ret.is_empty() {
                format!(
                    "<div class='alert alert-warning' role='alert'>No results found for <strong>{escaped_query}</strong>.</div>"
                )
            } else {
                let table = items2table(&ret);
                format!("<div class='row'>{table}</div>")
            };
            let html = format!("<h1>Results</h1>{form}{body}");
            let html = include_str!("../static/result.html").replace("%%RESULT%%", &html);
            Ok(Html(html).into_response())
        }
        _ => Ok(Json(ret).into_response()),
    }
}

/// Determine gender from a given name
#[utoipa::path(
    get,
    path = "/name_gender/{name}",
    responses((status = 200, description = "Gender information"))
)]
async fn name_gender(Path(name): Path<String>) -> Result<impl IntoResponse, StatusCode> {
    tracing::info!("name_gender: {name}");
    let statements = Person::name_gender(&name).await?;
    Ok(Json(statements))
}

/// Get administrative territorial entity (P131) for a coordinate
#[utoipa::path(
    get,
    path = "/P131/{latitude}/{longitude}",
    responses((status = 200, description = "P131 statements"))
)]
async fn p131(
    Path((latitude, longitude)): Path<(f64, f64)>,
) -> Result<impl IntoResponse, StatusCode> {
    tracing::info!("p131: {latitude}, {longitude}");
    let statements = Location::p131(latitude, longitude).await?;
    Ok(Json(statements))
}

/// Convert Wikipedia article titles between language editions
#[utoipa::path(
    post,
    path = "/change_wiki/{from}/{to}",
    responses((status = 200, description = "Converted titles"))
)]
async fn change_wiki(
    Path((from, to)): Path<(String, String)>,
    Json(payload): Json<serde_json::Value>,
) -> Result<impl IntoResponse, StatusCode> {
    tracing::info!("change_wiki: {from} -> {to}");
    let full_titles: Vec<String> = payload
        .as_array()
        .ok_or(StatusCode::BAD_REQUEST)?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let cw = crate::change_wiki::ChangeWiki::new(&from, full_titles);
    let results = cw.convert(&to).await.map_err(|e| {
        tracing::warn!("change_wiki conversion failed: {e}");
        StatusCode::NOT_FOUND
    })?;
    let results = json!(results);
    Ok(Json(results))
}

/// Cross-reference categories between Wikidata and Wikipedia
#[utoipa::path(
    get,
    path = "/cross_categories/{category_item}/{language}/{depth}",
    responses((status = 200, description = "Cross-category results"))
)]
async fn cross_cats(
    Path((category_item, language, depth)): Path<(String, String, u32)>,
) -> Result<impl IntoResponse, StatusCode> {
    tracing::info!("cross_cats: {category_item} lang={language} depth={depth}");
    let results = CrossCats::cross_cats(&category_item, depth, &language).await?;
    Ok(Json(results))
}

/// Look up a book by ISBN and generate a Wikidata item
#[utoipa::path(
    get,
    path = "/isbn/isbn/{isbn}",
    responses((status = 200, description = "Generated Wikidata item"))
)]
async fn isbn_isbn(Path(isbn): Path<String>) -> Result<impl IntoResponse, StatusCode> {
    tracing::info!("isbn_isbn: {isbn}");
    let mut isbn2wiki = ISBN2wiki::new(&isbn).ok_or_else(|| {
        tracing::warn!("isbn_isbn: invalid ISBN {isbn}");
        StatusCode::NOT_FOUND
    })?;
    isbn2wiki.retrieve().await.map_err(|e| {
        tracing::warn!("isbn_isbn retrieve failed for {isbn}: {e}");
        StatusCode::NOT_FOUND
    })?;
    let ret = isbn2wiki.generate_item().map_err(|e| {
        tracing::warn!("isbn_isbn generate_item failed for {isbn}: {e}");
        StatusCode::NOT_FOUND
    })?;
    let ret = json!({"item": ret});
    Ok(Json(ret))
}

/// Look up an existing Wikidata item by ISBN and generate a patch
#[utoipa::path(
    get,
    path = "/isbn/item/{item}",
    responses((status = 200, description = "Patch for the Wikidata item"))
)]
async fn isbn_item(Path(item): Path<String>) -> Result<impl IntoResponse, StatusCode> {
    tracing::info!("isbn_item: {item}");
    let mut isbn2wiki = ISBN2wiki::new_from_item(&item).await.ok_or_else(|| {
        tracing::warn!("isbn_item: invalid item {item}");
        StatusCode::NOT_FOUND
    })?;
    isbn2wiki.retrieve().await.map_err(|e| {
        tracing::error!("isbn_item retrieve failed for {item}: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let patch = isbn2wiki.generate_patch(&item).map_err(|e| {
        tracing::error!("isbn_item generate_patch failed for {item}: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let ret = patch.patch().to_owned();
    Ok(Json(ret))
}

/// Search VIAF for names and return matching Wikidata items
#[utoipa::path(
    get,
    path = "/viaf_search/{query}",
    responses((status = 200, description = "VIAF search results"))
)]
async fn viaf_search(Path(query): Path<String>) -> Result<impl IntoResponse, StatusCode> {
    tracing::info!("viaf_search: {query}");
    let results = crate::viaf::search_viaf_for_local_names(&query)
        .await
        .map_err(|e| {
            tracing::warn!("viaf_search failed for {query}: {e}");
            StatusCode::NOT_FOUND
        })?;
    Ok(Json(results))
}

/// Find potential references for a Wikidata item
#[utoipa::path(
    get,
    path = "/referee/{item}",
    responses((status = 200, description = "Potential references"))
)]
async fn referee(Path(item): Path<String>) -> Result<impl IntoResponse, StatusCode> {
    tracing::info!("referee: {item}");
    let results = Referee::new()
        .await
        .map_err(|e| {
            tracing::error!("referee::new() failed: {e}");
            StatusCode::NOT_FOUND
        })?
        .get_potential_references(&item)
        .await
        .map_err(|e| {
            tracing::warn!("referee get_potential_references failed for {item}: {e}");
            StatusCode::NOT_FOUND
        })?;
    Ok(Json(results))
}

/// Get the country for a location item in a given year
#[utoipa::path(
    get,
    path = "/country_year/{item}/{year}",
    responses((status = 200, description = "Country statements"))
)]
async fn country_year(
    Path((item, year)): Path<(String, i32)>,
) -> Result<impl IntoResponse, StatusCode> {
    tracing::info!("country_year: {item} year={year}");
    let statements = Location::country_for_location_and_date(&item, year).await?;
    Ok(Json(statements))
}

/// Get the country for a location item in a given year with a specific property
#[utoipa::path(
    get,
    path = "/country_year/{item}/{year}/{property}",
    responses((status = 200, description = "Country statements with custom property"))
)]
async fn country_year_property(
    Path((item, year, property)): Path<(String, i32, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    tracing::info!("country_year_property: {item} year={year} prop={property}");
    let mut statements = Location::country_for_location_and_date(&item, year).await?;
    for statement in &mut statements {
        statement.set_property(&property.to_uppercase());
    }
    Ok(Json(statements))
}

// ── Server (startup only) ────────────────────────────────────────────────────

impl Server {
    #![allow(clippy::print_stdout)]
    pub async fn start() -> Result<(), Box<dyn std::error::Error>> {
        tracing_subscriber::fmt::init();

        let cors = CorsLayer::new()
            .allow_origin(AllowOrigin::predicate(
                |origin: &axum::http::HeaderValue, _| {
                    let origin_str = match origin.to_str() {
                        Ok(s) => s,
                        Err(_) => return false,
                    };
                    // Only allow traffic from Wikipedia, Wikidata, or Toolforge
                    origin_str == "https://www.wikidata.org"
                        || origin_str == "https://wikidata.org"
                        || origin_str.ends_with(".wikipedia.org")
                        || origin_str.ends_with(".wikidata.org")
                        || origin_str.ends_with(".toolforge.org")
                },
            ))
            .allow_methods(Any)
            .allow_headers(Any);

        let rate_limit = RateLimiter::new(10, Duration::from_secs(1));

        let rate_limited_cached = Router::new()
            .route("/referee/:item", get(referee))
            .route("/isbn/item/:item", get(isbn_item))
            .route("/isbn/isbn/:isbn", get(isbn_isbn))
            .route("/viaf_search/:query", get(viaf_search))
            .layer(SetResponseHeaderLayer::overriding(
                CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=300"),
            ))
            .layer(middleware::from_fn_with_state(
                rate_limit,
                rate_limit_middleware,
            ));

        let cached = Router::new()
            .route("/initial_search/:query", get(initial_search))
            .route(
                "/cross_categories/:category_item/:language/:depth",
                get(cross_cats),
            )
            .layer(SetResponseHeaderLayer::overriding(
                CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=300"),
            ));

        let app = Router::new()
            .merge(SwaggerUi::new("/swagger-ui").url("/openapi.json", ApiDoc::openapi()))
            .route("/", get(root))
            .route("/healthz", get(healthz))
            .route("/metrics", get(crate::metrics::metrics_handler))
            .route("/P131/:latitude/:longitude", get(p131))
            .route("/name_gender/:name", get(name_gender))
            .route("/country_year/:item/:year", get(country_year))
            .route("/change_wiki/:from/:to", post(change_wiki))
            .route(
                "/country_year/:item/:year/:property",
                get(country_year_property),
            )
            .merge(rate_limited_cached)
            .merge(cached)
            .layer(middleware::from_fn(crate::metrics::middleware))
            .layer(TraceLayer::new_for_http())
            .layer(CompressionLayer::new().br(true).gzip(true))
            .layer(cors);

        let addr = Self::get_server_address();
        tracing::debug!("listening on {addr}");
        println!("listening on http://{addr}");
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!("startup complete");
        axum::serve(listener, app).await?;
        Ok(())
    }

    fn get_server_address() -> SocketAddr {
        let port: u16 = std::env::var("WD_INFERNAL_PORT")
            .map_or(8000, |port| port.as_str().parse::<u16>().unwrap_or(8000));

        let address = [0, 0, 0, 0];

        SocketAddr::from((address, port))
    }
}

// ── OpenAPI documentation ────────────────────────────────────────────────────

#[derive(OpenApi)]
#[openapi(
    paths(
        root,
        healthz,
        p131,
        name_gender,
        country_year,
        country_year_property,
        referee,
        viaf_search,
        isbn_item,
        isbn_isbn,
        initial_search,
        change_wiki,
        cross_cats,
        crate::metrics::metrics_handler,
    ),
    components(schemas()),
    info(title = "Wikidata Infernal API", version = "0.1.0")
)]
struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    // ── items2table ───────────────────────────────────────────────────────────

    #[test]
    fn test_items2table_empty_slice() {
        let html = items2table(&[]);
        // Must still produce a valid table shell
        assert!(html.contains("<table"), "should contain opening table tag");
        assert!(html.contains("<tbody></tbody>"), "tbody should be empty");
    }

    #[test]
    fn test_items2table_single_item() {
        let html = items2table(&["Q42".to_string()]);
        // Row number starts at 1
        assert!(html.contains("<th>1</th>"), "first row number should be 1");
        // The item ID appears as the q= attribute value
        assert!(
            html.contains("q='Q42'"),
            "item ID should appear as q= attribute"
        );
        // The item ID appears as link text
        assert!(html.contains(">Q42<"), "item ID should appear as link text");
        // The item ID appears inside <tt> for the raw ID column
        assert!(
            html.contains("<tt>Q42</tt>"),
            "item ID should appear in <tt>"
        );
    }

    #[test]
    fn test_items2table_multiple_items_numbered_correctly() {
        let items: Vec<String> = ["Q1", "Q2", "Q3"].iter().map(|s| s.to_string()).collect();
        let html = items2table(&items);
        assert!(
            html.contains("<th>1</th>"),
            "first row should be numbered 1"
        );
        assert!(
            html.contains("<th>2</th>"),
            "second row should be numbered 2"
        );
        assert!(
            html.contains("<th>3</th>"),
            "third row should be numbered 3"
        );
        assert!(
            !html.contains("<th>4</th>"),
            "should not have a fourth row number"
        );
    }

    #[test]
    fn test_items2table_all_items_present() {
        let items: Vec<String> = ["Q10", "Q20"].iter().map(|s| s.to_string()).collect();
        let html = items2table(&items);
        assert!(html.contains("Q10"), "Q10 should be present");
        assert!(html.contains("Q20"), "Q20 should be present");
    }

    #[test]
    fn test_items2table_table_structure() {
        let html = items2table(&["Q1".to_string()]);
        // Must have a striped Bootstrap table class
        assert!(
            html.contains("table-striped"),
            "table should have table-striped class"
        );
        // Must have thead with the six column headers
        assert!(html.contains("<thead>"), "should have thead");
        assert!(html.contains("Label"), "should have Label header");
        assert!(html.contains("Item"), "should have Item header");
        assert!(
            html.contains("Description"),
            "should have Description header"
        );
        assert!(html.contains("Born"), "should have Born header");
        assert!(html.contains("Died"), "should have Died header");
        // Must have tbody
        assert!(html.contains("<tbody>"), "should have tbody");
    }

    #[test]
    fn test_items2table_rows_separated_by_newlines() {
        let items: Vec<String> = ["Q1", "Q2"].iter().map(|s| s.to_string()).collect();
        let html = items2table(&items);
        // The two <tr> blocks must be joined by a newline (from .join("\n"))
        assert!(
            html.contains("</tr>\n<tr>"),
            "rows should be separated by newlines"
        );
    }

    // ── get_server_address ────────────────────────────────────────────────────

    #[test]
    fn test_get_server_address_does_not_panic() {
        let _ = Server::get_server_address();
    }

    #[test]
    fn test_get_server_address_is_ipv4() {
        let addr = Server::get_server_address();
        assert!(addr.is_ipv4(), "server address should always be IPv4");
    }

    #[test]
    fn test_get_server_address_binds_all_interfaces() {
        let addr = Server::get_server_address();
        assert_eq!(
            addr.ip().to_string(),
            "0.0.0.0",
            "server should always bind to all interfaces (0.0.0.0)"
        );
    }

    #[test]
    fn test_get_server_address_port_matches_env_or_default() {
        let expected: u16 = std::env::var("WD_INFERNAL_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8000);
        let addr = Server::get_server_address();
        assert_eq!(
            addr.port(),
            expected,
            "port must be WD_INFERNAL_PORT when set and valid, otherwise 8000"
        );
    }
}
