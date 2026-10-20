use axum::extract::MatchedPath;
use axum::{http::StatusCode, middleware::Next, response::IntoResponse};
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use utoipa;
use wikimisc::telemetry::{Telemetry, TelemetryConfig};

pub static TOTAL_REQUESTS: AtomicU64 = AtomicU64::new(0);
pub static ERROR_RESPONSES: AtomicU64 = AtomicU64::new(0);

/// Prometheus-compatible metrics endpoint
#[utoipa::path(
    get,
    path = "/metrics",
    responses((status = 200, description = "Metrics in Prometheus format"))
)]
pub async fn metrics_handler() -> impl IntoResponse {
    let total = TOTAL_REQUESTS.load(Ordering::Relaxed);
    let errors = ERROR_RESPONSES.load(Ordering::Relaxed);
    let body = format!(
        "# HELP http_requests_total Total number of HTTP requests\n\
         # TYPE http_requests_total counter\n\
         http_requests_total {total}\n\
         # HELP http_errors_total Total number of HTTP error responses\n\
         # TYPE http_errors_total counter\n\
         http_errors_total {errors}\n"
    );
    (StatusCode::OK, body).into_response()
}

/// Routes left out of the dashboard's request metrics.
const UNMONITORED_ROUTES: [&str; 2] = ["/healthz", "/metrics"];

/// The route template of a request (e.g. `/referee/:item`), never the raw
/// path, so it can be used as a metric label.
pub fn route_label(req: &axum::extract::Request) -> String {
    req.extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_string(), |p| p.as_str().to_string())
}

pub async fn middleware(
    req: axum::extract::Request,
    next: Next,
) -> Result<impl IntoResponse, StatusCode> {
    TOTAL_REQUESTS.fetch_add(1, Ordering::Relaxed);
    let route = route_label(&req);
    let start = Instant::now();
    let response = next.run(req).await;
    if !response.status().is_success() {
        ERROR_RESPONSES.fetch_add(1, Ordering::Relaxed);
    }
    if !UNMONITORED_ROUTES.contains(&route.as_str()) {
        wikimisc::telemetry::record_http_request(response.status().as_u16(), start.elapsed());
        wikimisc::metrics::counter!("wd_infernal_requests_total", "route" => route).increment(1);
    }
    Ok(response)
}

/// The optional `telemetry` block of `config.json`; defaults (token from
/// `$DASHBOARD_TOKEN`) if there is none or it is malformed.
fn telemetry_config(config: Option<&serde_json::Value>) -> TelemetryConfig {
    let Some(block) = config.map(|c| &c["telemetry"]).filter(|b| !b.is_null()) else {
        return TelemetryConfig::default();
    };
    TelemetryConfig::deserialize(block).unwrap_or_else(|e| {
        tracing::warn!("ignoring malformed telemetry config: {e}");
        TelemetryConfig::default()
    })
}

/// Starts pushing metrics to the service dashboard. Telemetry is optional:
/// a missing token or any other error only gets logged.
pub fn start_telemetry() {
    let config = crate::load_config().unwrap_or_else(|e| {
        tracing::warn!("cannot read config.json: {e}");
        None
    });
    Telemetry::builder("web")
        .config(&telemetry_config(config.as_ref()))
        .version(env!("CARGO_PKG_VERSION"))
        // `wikimisc::server::serve` drains on SIGTERM and then sends the final push.
        .handle_signals(false)
        .install_or_log();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_telemetry_config_reads_block() {
        let config = json!({"telemetry": {"url": "https://example.org/ingest", "token": "secret"}});
        let tc = telemetry_config(Some(&config));
        assert_eq!(tc.url.as_deref(), Some("https://example.org/ingest"));
        assert_eq!(tc.token.as_deref(), Some("secret"));
    }

    #[test]
    fn test_telemetry_config_defaults_without_block() {
        for config in [
            None,
            Some(json!({"wikidata": {}})),
            Some(json!({"telemetry": 42})),
        ] {
            let tc = telemetry_config(config.as_ref());
            assert!(tc.url.is_none() && tc.token.is_none());
        }
    }
}
