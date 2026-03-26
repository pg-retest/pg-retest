use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::web::state::AppState;

#[derive(Deserialize)]
pub struct SynthesizeConfig {
    pub source_db: String,
    #[serde(default)]
    pub sessions: Option<u32>,
    #[serde(default)]
    pub think_time_ms: Option<u64>,
    #[serde(default)]
    pub scale_data: Option<f64>,
    #[serde(default)]
    pub seed: Option<u64>,
}

/// GET /api/v1/workloads
pub async fn list_workloads(State(state): State<AppState>) -> Json<serde_json::Value> {
    let db = state.db.lock().await;
    match crate::web::db::list_workloads(&db) {
        Ok(workloads) => Json(json!({ "workloads": workloads })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// GET /api/v1/workloads/:id
pub async fn get_workload(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = state.db.lock().await;
    match crate::web::db::get_workload(&db, &id) {
        Ok(Some(w)) => Ok(Json(json!(w))),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// GET /api/v1/workloads/:id/inspect
pub async fn inspect_workload(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = state.db.lock().await;
    let workload = match crate::web::db::get_workload(&db, &id) {
        Ok(Some(w)) => w,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    drop(db);

    let path = std::path::Path::new(&workload.file_path);
    match crate::profile::io::read_profile(path) {
        Ok(profile) => {
            let classification = crate::classify::classify_workload(&profile);
            Ok(Json(json!({
                "profile": profile,
                "classification": classification,
            })))
        }
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// POST /api/v1/workloads/import — Import an existing .wkl file
pub async fn import_workload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut file_data = None;
    let mut file_name = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            file_name = field.file_name().map(|s| s.to_string());
            file_data = field.bytes().await.ok().map(|b| b.to_vec());
        }
    }

    let data = file_data.ok_or(StatusCode::BAD_REQUEST)?;
    let name = file_name.unwrap_or_else(|| "uploaded.wkl".to_string());
    let id = uuid::Uuid::new_v4().to_string();

    // Save .wkl file to data dir
    let wkl_dir = state.data_dir.join("workloads");
    std::fs::create_dir_all(&wkl_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let file_path = wkl_dir.join(format!("{id}.wkl"));
    std::fs::write(&file_path, &data).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Read profile to extract metadata
    let profile =
        crate::profile::io::read_profile(&file_path).map_err(|_| StatusCode::BAD_REQUEST)?;

    let classification = crate::classify::classify_workload(&profile);

    let row = crate::web::db::WorkloadRow {
        id: id.clone(),
        name: name.trim_end_matches(".wkl").to_string(),
        file_path: file_path.to_string_lossy().to_string(),
        source_type: Some(profile.capture_method.clone()),
        source_host: Some(profile.source_host.clone()),
        captured_at: Some(profile.captured_at.to_rfc3339()),
        total_sessions: Some(profile.metadata.total_sessions as i64),
        total_queries: Some(profile.metadata.total_queries as i64),
        capture_duration_us: Some(profile.metadata.capture_duration_us as i64),
        classification: Some(serde_json::to_string(&classification).unwrap_or_default()),
        created_at: None,
    };

    let db = state.db.lock().await;
    crate::web::db::insert_workload(&db, &row).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({ "id": id, "workload": row })))
}

/// POST /api/v1/workloads/upload — Upload a log file and run capture
pub async fn upload_workload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut file_data = None;
    let mut file_name = None;
    let mut source_type = "pg-csv".to_string();
    let mut source_host = "uploaded".to_string();
    let mut mask_values = false;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                file_name = field.file_name().map(|s| s.to_string());
                file_data = field.bytes().await.ok().map(|b| b.to_vec());
            }
            "source_type" => {
                if let Ok(text) = field.text().await {
                    source_type = text;
                }
            }
            "source_host" => {
                if let Ok(text) = field.text().await {
                    source_host = text;
                }
            }
            "mask_values" => {
                if let Ok(text) = field.text().await {
                    mask_values = text == "true";
                }
            }
            _ => {}
        }
    }

    let data = file_data.ok_or(StatusCode::BAD_REQUEST)?;
    let name = file_name.unwrap_or_else(|| "uploaded_log".to_string());
    let id = uuid::Uuid::new_v4().to_string();

    // Save uploaded log to temp file
    let tmp_dir = state.data_dir.join("tmp");
    std::fs::create_dir_all(&tmp_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let log_path = tmp_dir.join(format!("{id}.log"));
    std::fs::write(&log_path, &data).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Run capture
    let mut profile = match source_type.as_str() {
        "pg-csv" => {
            use crate::capture::csv_log::CsvLogCapture;
            CsvLogCapture
                .capture_from_file(&log_path, &source_host, "unknown")
                .map_err(|_| StatusCode::BAD_REQUEST)?
        }
        "mysql-slow" => {
            use crate::capture::mysql_slow::MysqlSlowLogCapture;
            MysqlSlowLogCapture
                .capture_from_file(&log_path, &source_host, true)
                .map_err(|_| StatusCode::BAD_REQUEST)?
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    if mask_values {
        for session in &mut profile.sessions {
            for query in &mut session.queries {
                query.sql = crate::capture::masking::mask_sql_literals(&query.sql);
            }
        }
    }

    // Save .wkl file
    let wkl_dir = state.data_dir.join("workloads");
    std::fs::create_dir_all(&wkl_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let wkl_path = wkl_dir.join(format!("{id}.wkl"));
    crate::profile::io::write_profile(&wkl_path, &profile)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let classification = crate::classify::classify_workload(&profile);

    let row = crate::web::db::WorkloadRow {
        id: id.clone(),
        name: name
            .trim_end_matches(".csv")
            .trim_end_matches(".log")
            .to_string(),
        file_path: wkl_path.to_string_lossy().to_string(),
        source_type: Some(source_type),
        source_host: Some(source_host),
        captured_at: Some(profile.captured_at.to_rfc3339()),
        total_sessions: Some(profile.metadata.total_sessions as i64),
        total_queries: Some(profile.metadata.total_queries as i64),
        capture_duration_us: Some(profile.metadata.capture_duration_us as i64),
        classification: Some(serde_json::to_string(&classification).unwrap_or_default()),
        created_at: None,
    };

    let db = state.db.lock().await;
    crate::web::db::insert_workload(&db, &row).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Clean up temp file
    let _ = std::fs::remove_file(&log_path);

    Ok(Json(json!({ "id": id, "workload": row })))
}

/// POST /api/v1/workloads/{id}/compile
/// Compiles a workload: strips response_values, validates IDs, produces a deterministic .wkl
pub async fn compile_workload(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // 1. Get the workload metadata from SQLite
    let db = state.db.lock().await;
    let workload = match crate::web::db::get_workload(&db, &id) {
        Ok(Some(w)) => w,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    drop(db);

    // 2. Load the profile from disk
    let path = std::path::Path::new(&workload.file_path);
    let profile =
        crate::profile::io::read_profile(path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 3. Compile the workload
    let (compiled, stats) = crate::correlate::compile::compile_workload(profile).map_err(|e| {
        tracing::warn!("Compile failed for workload {}: {}", id, e);
        StatusCode::BAD_REQUEST
    })?;

    // 4. Write the compiled profile to a new file
    let new_id = uuid::Uuid::new_v4().to_string();
    let wkl_dir = state.data_dir.join("workloads");
    std::fs::create_dir_all(&wkl_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let compiled_path = wkl_dir.join(format!("{new_id}.wkl"));
    crate::profile::io::write_profile(&compiled_path, &compiled)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 5. Import the compiled workload into SQLite
    let classification = crate::classify::classify_workload(&compiled);
    let compiled_name = format!("{}-compiled", workload.name);

    let row = crate::web::db::WorkloadRow {
        id: new_id.clone(),
        name: compiled_name,
        file_path: compiled_path.to_string_lossy().to_string(),
        source_type: Some(compiled.capture_method.clone()),
        source_host: Some(compiled.source_host.clone()),
        captured_at: Some(compiled.captured_at.to_rfc3339()),
        total_sessions: Some(compiled.metadata.total_sessions as i64),
        total_queries: Some(compiled.metadata.total_queries as i64),
        capture_duration_us: Some(compiled.metadata.capture_duration_us as i64),
        classification: Some(serde_json::to_string(&classification).unwrap_or_default()),
        created_at: None,
    };

    let db = state.db.lock().await;
    crate::web::db::insert_workload(&db, &row).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({
        "id": new_id,
        "workload": row,
        "stats": {
            "queries_with_responses": stats.queries_with_responses,
            "unique_captured_ids": stats.unique_captured_ids,
            "queries_referencing_ids": stats.queries_referencing_ids,
            "total_id_references": stats.total_id_references,
        }
    })))
}

/// DELETE /api/v1/workloads/:id
pub async fn delete_workload(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = state.db.lock().await;

    // Get file path to delete file too
    if let Ok(Some(w)) = crate::web::db::get_workload(&db, &id) {
        let _ = std::fs::remove_file(&w.file_path);
    }

    match crate::web::db::delete_workload(&db, &id) {
        Ok(true) => Ok(Json(json!({ "deleted": true }))),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// POST /api/v1/workloads/{id}/synthesize
///
/// Shells out to the Python synthesizer to generate a synthetic workload from
/// a captured one. The generated .wkl is imported into SQLite automatically.
pub async fn synthesize_workload(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(config): Json<SynthesizeConfig>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // 1. Get the workload metadata from SQLite
    let db = state.db.lock().await;
    let workload = match crate::web::db::get_workload(&db, &id) {
        Ok(Some(w)) => w,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    drop(db);

    // 2. Locate the Python synthesizer script
    let script_path = find_synthesizer_script().ok_or_else(|| {
        tracing::warn!("synthesize-workload.py not found");
        StatusCode::NOT_FOUND
    })?;

    // 3. Prepare output paths
    let new_id = uuid::Uuid::new_v4().to_string();
    let wkl_dir = state.data_dir.join("workloads");
    std::fs::create_dir_all(&wkl_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let output_wkl = wkl_dir.join(format!("{new_id}.wkl"));
    let output_data = state
        .data_dir
        .join("tmp")
        .join(format!("{new_id}-data.sql"));
    std::fs::create_dir_all(output_data.parent().unwrap())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let seed = config.seed.unwrap_or_else(rand::random::<u64>);

    // 4. Shell out to Python
    let mut cmd = tokio::process::Command::new("python3");
    cmd.arg(&script_path)
        .arg("--input")
        .arg(&workload.file_path)
        .arg("--source-db")
        .arg(&config.source_db)
        .arg("--output-workload")
        .arg(&output_wkl)
        .arg("--output-data")
        .arg(&output_data)
        .arg("--seed")
        .arg(seed.to_string());

    if let Some(sessions) = config.sessions {
        cmd.arg("--sessions").arg(sessions.to_string());
    }
    if let Some(think_time_ms) = config.think_time_ms {
        cmd.arg("--think-time-ms").arg(think_time_ms.to_string());
    }
    if let Some(scale_data) = config.scale_data {
        cmd.arg("--scale-data").arg(scale_data.to_string());
    }

    let output = cmd.output().await.map_err(|e| {
        tracing::warn!("Failed to execute synthesizer: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("Synthesizer failed: {stderr}");
        return Ok(Json(json!({
            "error": format!("Synthesizer failed: {}", stderr.trim())
        })));
    }

    // 5. Import the generated .wkl into SQLite
    let profile = crate::profile::io::read_profile(&output_wkl).map_err(|e| {
        tracing::warn!("Failed to read synthesized profile: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let classification = crate::classify::classify_workload(&profile);
    let synth_name = format!("{}-synthesized", workload.name);

    let row = crate::web::db::WorkloadRow {
        id: new_id.clone(),
        name: synth_name,
        file_path: output_wkl.to_string_lossy().to_string(),
        source_type: Some("synthesized".to_string()),
        source_host: Some(workload.source_host.unwrap_or_default()),
        captured_at: Some(chrono::Utc::now().to_rfc3339()),
        total_sessions: Some(profile.metadata.total_sessions as i64),
        total_queries: Some(profile.metadata.total_queries as i64),
        capture_duration_us: Some(profile.metadata.capture_duration_us as i64),
        classification: Some(serde_json::to_string(&classification).unwrap_or_default()),
        created_at: None,
    };

    let db = state.db.lock().await;
    crate::web::db::insert_workload(&db, &row).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let stdout_msg = String::from_utf8_lossy(&output.stdout);

    Ok(Json(json!({
        "id": new_id,
        "workload": row,
        "data_sql_path": output_data.to_string_lossy(),
        "seed": seed,
        "synthesizer_output": stdout_msg.trim(),
    })))
}

/// Find the synthesize-workload.py script in known locations.
fn find_synthesizer_script() -> Option<std::path::PathBuf> {
    // 1. Check env var
    if let Ok(demo_dir) = std::env::var("PG_RETEST_DEMO_DIR") {
        let p = std::path::PathBuf::from(demo_dir).join("synthesize-workload.py");
        if p.exists() {
            return Some(p);
        }
    }
    // 2. Check relative to cwd
    let cwd_path = std::path::PathBuf::from("demo/synthesize-workload.py");
    if cwd_path.exists() {
        return Some(cwd_path);
    }
    // 3. Check relative to binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let p = parent.join("demo/synthesize-workload.py");
            if p.exists() {
                return Some(p);
            }
            // Also check one level up (for target/debug layout)
            let p = parent
                .parent()
                .and_then(|pp| pp.parent())
                .map(|pp| pp.join("demo/synthesize-workload.py"));
            if let Some(p) = p {
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    None
}
