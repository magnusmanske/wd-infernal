use crate::initial_search::InitialSearch;
use crate::isbn::ISBN2wiki;
use crate::person::Person;
use crate::referee::Referee;
use crate::{crosscats::CrossCats, location::Location};
use axum::extract::Query;
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
use std::net::SocketAddr;
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use wikibase_rest_api::Patch;

#[derive(Deserialize)]
struct Format {
    format: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Server;

impl Server {
    #![allow(clippy::print_stdout)]
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
            .route("/change_wiki/:from/:to", post(Self::change_wiki))
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
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    }

    fn get_server_address() -> SocketAddr {
        let port: u16 = std::env::var("WD_INFERNAL_PORT")
            .map_or(8000, |port| port.as_str().parse::<u16>().unwrap_or(8000));

        let address = [0, 0, 0, 0];
        // TODOO env::var("WD_INFERNAL_ADDRESS")

        SocketAddr::from((address, port))
    }

    async fn root() -> impl IntoResponse {
        let ret = include_str!("../static/root.html");
        Html(ret)
    }

    fn items2table(items: &[String]) -> String {
        let mut html = items
            .iter()
            .enumerate()
            .map(|(num, q)| {
                format!(
                    "<tr><th>{}</th><td><a q='{q}'>{q}</a></td><td><tt>{q}</tt></td><td class='desc' data-q='{q}'></td><td class='birth' data-q='{q}'></td><td class='death' data-q='{q}'></td></tr>",
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

    async fn initial_search(
        Path(query): Path<String>,
        params: Query<Format>,
    ) -> Result<impl IntoResponse, StatusCode> {
        let ret = InitialSearch::run(&query)
            .await
            .map_err(|_e| StatusCode::BAD_REQUEST)?;
        match params.format.as_deref() {
            Some("html") => {
                let table = Self::items2table(&ret);
                let escaped_query = query.replace('&', "&amp;").replace('"', "&quot;");
                let form = format!(
                    "<form id='search-form' class='mb-3'>\
                        <div class='input-group'>\
                            <input type='text' id='search-input' class='form-control' value=\"{escaped_query}\" placeholder='Search name'>\
                            <div class='input-group-append'>\
                                <button type='submit' class='btn btn-primary'>Search</button>\
                            </div>\
                        </div>\
                    </form>\
                    <script>\
                        document.getElementById('search-form').addEventListener('submit',function(e){{\
                            e.preventDefault();\
                            var n=document.getElementById('search-input').value.trim();\
                            if(n)window.location.href='/initial_search/'+encodeURIComponent(n)+'?format=html';\
                        }});\
                    </script>"
                );
                let html = format!("<h1>Results</h1>{form}<div class='row'>{table}</div>");
                let html = include_str!("../static/result.html").replace("%%RESULT%%", &html);
                Ok(Html(html).into_response())
            }
            _ => Ok(Json(ret).into_response()),
        }
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

    // Pass "from" and "to" wikis as parameters
    // Pass a JSON array of full titles as POST payload
    async fn change_wiki(
        Path((from, to)): Path<(String, String)>,
        Json(payload): Json<serde_json::Value>,
    ) -> Result<impl IntoResponse, StatusCode> {
        let full_titles: Vec<String> = payload
            .as_array()
            .ok_or(StatusCode::BAD_REQUEST)?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let cw = crate::change_wiki::ChangeWiki::new(&from, full_titles);
        let results = cw.convert(&to).await.map_err(|_| StatusCode::NOT_FOUND)?;
        let results = json!(results);
        Ok(Json(results))
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
        let mut isbn2wiki = ISBN2wiki::new_from_item(&item)
            .await
            .ok_or(StatusCode::NOT_FOUND)?;
        isbn2wiki
            .retrieve()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let patch = isbn2wiki
            .generate_patch(&item)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── items2table ───────────────────────────────────────────────────────────

    #[test]
    fn test_items2table_empty_slice() {
        let html = Server::items2table(&[]);
        // Must still produce a valid table shell
        assert!(html.contains("<table"), "should contain opening table tag");
        assert!(html.contains("<tbody></tbody>"), "tbody should be empty");
    }

    #[test]
    fn test_items2table_single_item() {
        let html = Server::items2table(&["Q42".to_string()]);
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
        let html = Server::items2table(&items);
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
        let html = Server::items2table(&items);
        assert!(html.contains("Q10"), "Q10 should be present");
        assert!(html.contains("Q20"), "Q20 should be present");
    }

    #[test]
    fn test_items2table_table_structure() {
        let html = Server::items2table(&["Q1".to_string()]);
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
        let html = Server::items2table(&items);
        // The two <tr> blocks must be joined by a newline (from .join("\n"))
        assert!(
            html.contains("</tr>\n<tr>"),
            "rows should be separated by newlines"
        );
    }

    // ── get_server_address ────────────────────────────────────────────────────
    // The crate forbids unsafe code, so set_var/remove_var cannot be called in
    // tests. We therefore test only properties that are independent of the env
    // var, plus one test that reads (but does not write) the env var to derive
    // the expected value.

    #[test]
    fn test_get_server_address_does_not_panic() {
        // Smoke-test: the function must not panic regardless of the current env var state
        let _ = Server::get_server_address();
    }

    #[test]
    fn test_get_server_address_is_ipv4() {
        let addr = Server::get_server_address();
        assert!(addr.is_ipv4(), "server address should always be IPv4");
    }

    #[test]
    fn test_get_server_address_binds_all_interfaces() {
        // The bind address is hardcoded as [0, 0, 0, 0] — independent of any env var
        let addr = Server::get_server_address();
        assert_eq!(
            addr.ip().to_string(),
            "0.0.0.0",
            "server should always bind to all interfaces (0.0.0.0)"
        );
    }

    #[test]
    fn test_get_server_address_port_matches_env_or_default() {
        // Read (but do not write) the env var to derive the expected port, mirroring
        // the same logic used inside get_server_address().
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
