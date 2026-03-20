//! Minimal Axum HTTP server for controlling a persistent proxy from the CLI.
//!
//! Provides three routes:
//! - `GET /status` — proxy and capture status
//! - `POST /start-capture` — begin a new capture session
//! - `POST /stop-capture` — stop capturing and optionally write the profile

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::sync::{oneshot, RwLock};

/// Commands sent from the control endpoint to the proxy's capture manager.
pub enum CaptureCommand {
    Start {
        reply: oneshot::Sender<Result<String, String>>,
    },
    Stop {
        output: Option<PathBuf>,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
}

/// Shared state for the control endpoint.
pub struct ControlState {
    pub running: bool,
    pub capturing: bool,
    pub capture_id: Option<String>,
    pub active_sessions: u64,
    pub total_queries: u64,
    pub started_at: Instant,
    pub capture_cmd_tx: Option<tokio::sync::mpsc::UnboundedSender<CaptureCommand>>,
}

type SharedState = Arc<RwLock<ControlState>>;

/// Build the control router with all three endpoints.
pub fn build_control_router(state: SharedState) -> Router {
    Router::new()
        .route("/status", get(status_handler))
        .route("/start-capture", post(start_capture_handler))
        .route("/stop-capture", post(stop_capture_handler))
        .with_state(state)
}

async fn status_handler(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let s = state.read().await;
    Json(serde_json::json!({
        "running": s.running,
        "capturing": s.capturing,
        "capture_id": s.capture_id,
        "active_sessions": s.active_sessions,
        "total_queries": s.total_queries,
        "uptime_secs": s.started_at.elapsed().as_secs(),
    }))
}

async fn start_capture_handler(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let s = state.read().await;

    if s.capturing {
        return Json(serde_json::json!({ "error": "Already capturing" }));
    }

    let tx = match &s.capture_cmd_tx {
        Some(tx) => tx.clone(),
        None => return Json(serde_json::json!({ "error": "Proxy not configured for capture" })),
    };

    // Drop the read lock before sending the command (the receiver may need to
    // write-lock the state to update it).
    drop(s);

    let (reply_tx, reply_rx) = oneshot::channel();
    if tx.send(CaptureCommand::Start { reply: reply_tx }).is_err() {
        return Json(serde_json::json!({ "error": "Proxy not responding" }));
    }

    match reply_rx.await {
        Ok(Ok(capture_id)) => Json(serde_json::json!({
            "ok": true,
            "capture_id": capture_id,
        })),
        Ok(Err(e)) => Json(serde_json::json!({ "error": e })),
        Err(_) => Json(serde_json::json!({ "error": "Proxy not responding" })),
    }
}

#[derive(Deserialize, Default)]
struct StopBody {
    output: Option<PathBuf>,
}

async fn stop_capture_handler(
    State(state): State<SharedState>,
    body: Option<Json<StopBody>>,
) -> Json<serde_json::Value> {
    let s = state.read().await;

    if !s.capturing {
        return Json(serde_json::json!({ "error": "Not currently capturing" }));
    }

    let tx = match &s.capture_cmd_tx {
        Some(tx) => tx.clone(),
        None => return Json(serde_json::json!({ "error": "Proxy not configured for capture" })),
    };

    drop(s);

    let output = body.and_then(|b| b.0.output);

    let (reply_tx, reply_rx) = oneshot::channel();
    if tx
        .send(CaptureCommand::Stop {
            output,
            reply: reply_tx,
        })
        .is_err()
    {
        return Json(serde_json::json!({ "error": "Proxy not responding" }));
    }

    match reply_rx.await {
        Ok(Ok(result)) => Json(result),
        Ok(Err(e)) => Json(serde_json::json!({ "error": e })),
        Err(_) => Json(serde_json::json!({ "error": "Proxy not responding" })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_state(capturing: bool, with_tx: bool) -> SharedState {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        Arc::new(RwLock::new(ControlState {
            running: true,
            capturing,
            capture_id: if capturing {
                Some("cap-123".to_string())
            } else {
                None
            },
            active_sessions: 3,
            total_queries: 42,
            started_at: Instant::now(),
            capture_cmd_tx: if with_tx { Some(tx) } else { None },
        }))
    }

    #[tokio::test]
    async fn test_status_endpoint() {
        let state = test_state(false, true);
        let app = build_control_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["running"], true);
        assert_eq!(json["capturing"], false);
        assert_eq!(json["active_sessions"], 3);
        assert_eq!(json["total_queries"], 42);
    }

    #[tokio::test]
    async fn test_start_capture_already_capturing() {
        let state = test_state(true, true);
        let app = build_control_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/start-capture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "Already capturing");
    }

    #[tokio::test]
    async fn test_stop_capture_not_capturing() {
        let state = test_state(false, true);
        let app = build_control_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/stop-capture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "Not currently capturing");
    }

    #[tokio::test]
    async fn test_start_capture_no_tx() {
        let state = Arc::new(RwLock::new(ControlState {
            running: true,
            capturing: false,
            capture_id: None,
            active_sessions: 0,
            total_queries: 0,
            started_at: Instant::now(),
            capture_cmd_tx: None,
        }));
        let app = build_control_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/start-capture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "Proxy not configured for capture");
    }
}
