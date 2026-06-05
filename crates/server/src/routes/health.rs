use axum::Json;
use clipper_core::models::HealthResponse;

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {})
}
