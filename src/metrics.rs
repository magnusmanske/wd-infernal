use axum::{http::StatusCode, middleware::Next, response::IntoResponse};
use std::sync::atomic::{AtomicU64, Ordering};
use utoipa;

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

pub async fn middleware(
    req: axum::extract::Request,
    next: Next,
) -> Result<impl IntoResponse, StatusCode> {
    TOTAL_REQUESTS.fetch_add(1, Ordering::Relaxed);
    let response = next.run(req).await;
    if !response.status().is_success() {
        ERROR_RESPONSES.fetch_add(1, Ordering::Relaxed);
    }
    Ok(response)
}
