use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::json;
use tokio_postgres::NoTls;

use crate::web::state::AppState;

#[derive(Deserialize)]
pub struct DriftCheckConfig {
    pub db_a: String,
    pub db_b: String,
}

/// POST /api/v1/drift-check
///
/// Connects to two databases and compares row counts for all tables in the
/// public schema. Returns per-table match/drift status and a summary.
pub async fn drift_check(
    State(_state): State<AppState>,
    Json(config): Json<DriftCheckConfig>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Connect to both databases concurrently
    let (conn_a, conn_b) = tokio::try_join!(connect_db(&config.db_a), connect_db(&config.db_b),)
        .map_err(|e| {
            tracing::warn!("Drift check connection error: {e}");
            StatusCode::BAD_GATEWAY
        })?;

    // Get table list from db_a (public schema)
    let tables = get_public_tables(&conn_a).await.map_err(|e| {
        tracing::warn!("Failed to list tables from db_a: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut table_results = Vec::new();
    let mut matching = 0u64;
    let mut drifted = 0u64;

    for table_name in &tables {
        let count_a = get_row_count(&conn_a, table_name).await;
        let count_b = get_row_count(&conn_b, table_name).await;

        match (count_a, count_b) {
            (Ok(a), Ok(b)) => {
                let diff = b - a;
                let status = if diff == 0 { "MATCH" } else { "DRIFT" };
                if diff == 0 {
                    matching += 1;
                } else {
                    drifted += 1;
                }
                let mut entry = json!({
                    "name": table_name,
                    "db_a_count": a,
                    "db_b_count": b,
                    "status": status,
                });
                if diff != 0 {
                    entry["diff"] = json!(diff);
                }
                table_results.push(entry);
            }
            (Ok(a), Err(_)) => {
                drifted += 1;
                table_results.push(json!({
                    "name": table_name,
                    "db_a_count": a,
                    "db_b_count": null,
                    "status": "DRIFT",
                    "error": "Table missing or inaccessible in db_b"
                }));
            }
            (Err(_), Ok(b)) => {
                drifted += 1;
                table_results.push(json!({
                    "name": table_name,
                    "db_a_count": null,
                    "db_b_count": b,
                    "status": "DRIFT",
                    "error": "Table missing or inaccessible in db_a"
                }));
            }
            (Err(ea), Err(eb)) => {
                drifted += 1;
                table_results.push(json!({
                    "name": table_name,
                    "db_a_count": null,
                    "db_b_count": null,
                    "status": "ERROR",
                    "error": format!("db_a: {ea}, db_b: {eb}")
                }));
            }
        }
    }

    let total = matching + drifted;

    Ok(Json(json!({
        "tables": table_results,
        "summary": {
            "total": total,
            "matching": matching,
            "drifted": drifted,
        }
    })))
}

async fn connect_db(conn_str: &str) -> Result<tokio_postgres::Client, tokio_postgres::Error> {
    let (client, connection) = tokio_postgres::connect(conn_str, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::warn!("Database connection error: {e}");
        }
    });
    Ok(client)
}

async fn get_public_tables(
    client: &tokio_postgres::Client,
) -> Result<Vec<String>, tokio_postgres::Error> {
    let rows = client
        .query(
            "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
            &[],
        )
        .await?;
    Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
}

async fn get_row_count(
    client: &tokio_postgres::Client,
    table_name: &str,
) -> Result<i64, tokio_postgres::Error> {
    // Use identifier quoting to prevent SQL injection
    let query = format!(
        "SELECT COUNT(*) FROM \"{}\"",
        table_name.replace('"', "\"\"")
    );
    let row = client.query_one(&query, &[]).await?;
    Ok(row.get::<_, i64>(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drift_config_deserialize() {
        let json_str =
            r#"{"db_a": "host=localhost dbname=test", "db_b": "host=localhost dbname=test2"}"#;
        let config: DriftCheckConfig = serde_json::from_str(json_str).unwrap();
        assert_eq!(config.db_a, "host=localhost dbname=test");
        assert_eq!(config.db_b, "host=localhost dbname=test2");
    }
}
