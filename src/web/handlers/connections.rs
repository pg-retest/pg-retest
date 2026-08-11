use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::web::state::AppState;

#[derive(Deserialize)]
pub struct SaveConnectionRequest {
    pub label: String,
    pub conn_string: String,
}

/// GET /api/v1/connections
pub async fn list_connections(State(state): State<AppState>) -> Json<serde_json::Value> {
    let db = state.db.lock().await;
    match crate::web::db::list_connections(&db) {
        Ok(connections) => Json(json!({ "connections": connections })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// POST /api/v1/connections — saves a new labeled connection, or updates an existing
/// label's conn_string (labels are unique).
pub async fn save_connection(
    State(state): State<AppState>,
    Json(body): Json<SaveConnectionRequest>,
) -> Json<serde_json::Value> {
    let label = body.label.trim();
    let conn_string = body.conn_string.trim();
    if label.is_empty() || conn_string.is_empty() {
        return Json(json!({ "error": "label and conn_string are required" }));
    }

    let db = state.db.lock().await;
    match crate::web::db::upsert_connection(&db, label, conn_string) {
        Ok(()) => Json(json!({ "status": "ok" })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// DELETE /api/v1/connections/{label}
pub async fn delete_connection(
    State(state): State<AppState>,
    Path(label): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = state.db.lock().await;
    match crate::web::db::delete_connection_by_label(&db, &label) {
        Ok(true) => Ok(Json(json!({ "status": "ok" }))),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}
