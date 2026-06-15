//! `LiveDiffOracle` — the truest oracle: run the ORIGINAL query on a real MySQL and the
//! CANDIDATE translation on PostgreSQL, then diff the result sets. This is the actual
//! migration-validation primitive ("your real MySQL query and its translation return the
//! same rows"). It implements the same `Oracle` trait as `GoldenOracle`, so it drops
//! into the verified-search engine with zero engine change — only the source of truth
//! differs (live MySQL execution instead of an author-verified reference).
//!
//! MySQL is reached via its CLI (no Rust driver dependency — same external-tool pattern
//! pg-retest uses for the aws/bedrock CLIs). Configure the invocation via
//! `PG_RETEST_MYSQL_CMD`, e.g. (running the client from the cached image):
//!   PG_RETEST_MYSQL_CMD="docker exec <container> mysql -uroot -proot -N --batch --raw db"
//! The oracle appends `-e <sql>`. PostgreSQL is reached via tokio-postgres as usual.
//!
//! Cross-engine result normalization (`super::normalize`) reconciles the two engines'
//! text renderings (NULL vs '', record quoting vs TSV). Assumes the corpus avoids values
//! containing tabs/newlines and float/decimal precision differences (Phase-2 follow-on).

use async_trait::async_trait;
use tokio::process::Command;
use tokio_postgres::{Client, NoTls};

use super::normalize::{is_ordered, join_cells, parse_mysql_row, parse_pg_record, rows_equivalent};
use super::{Oracle, Verdict};

pub struct LiveDiffOracle {
    pg: Client,
    mysql_argv: Vec<String>,
}

impl LiveDiffOracle {
    pub async fn connect(pg_conn: &str, mysql_argv: Vec<String>) -> Result<Self, String> {
        let (pg, connection) = tokio_postgres::connect(pg_conn, NoTls)
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Self { pg, mysql_argv })
    }

    /// Parse the MySQL invocation from `PG_RETEST_MYSQL_CMD` (whitespace-split argv).
    pub fn mysql_argv_from_env() -> Option<Vec<String>> {
        let argv: Vec<String> = std::env::var("PG_RETEST_MYSQL_CMD")
            .ok()?
            .split_whitespace()
            .map(String::from)
            .collect();
        (!argv.is_empty()).then_some(argv)
    }

    pub async fn batch_pg(&self, sql: &str) -> Result<(), tokio_postgres::Error> {
        self.pg.batch_execute(sql).await
    }

    /// Run SQL on MySQL via the configured CLI, returning raw stdout.
    pub async fn mysql_exec(&self, sql: &str) -> Result<String, String> {
        let out = Command::new(&self.mysql_argv[0])
            .args(&self.mysql_argv[1..])
            .arg("-e")
            .arg(sql)
            .output()
            .await
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    async fn mysql_canonical(&self, sql: &str) -> Result<Vec<String>, String> {
        let out = self.mysql_exec(sql).await?;
        Ok(out
            .lines()
            .map(|l| join_cells(&parse_mysql_row(l)))
            .collect())
    }

    async fn pg_canonical(&self, sql: &str) -> Result<Vec<String>, String> {
        let wrapped = format!("SELECT _s::text AS r FROM ( {sql} ) AS _s");
        let rows = self
            .pg
            .query(&wrapped, &[])
            .await
            .map_err(|e| e.to_string())?;
        rows.iter()
            .map(|row| {
                let rec: Option<String> = row.try_get("r").map_err(|e| e.to_string())?;
                Ok(join_cells(&parse_pg_record(&rec.unwrap_or_default())))
            })
            .collect()
    }
}

#[async_trait]
impl Oracle for LiveDiffOracle {
    /// `reference_sql` is the ORIGINAL MySQL query (truth = MySQL execution);
    /// `candidate_sql` is the candidate PostgreSQL translation.
    async fn verify(&self, candidate_sql: &str, reference_sql: &str) -> Verdict {
        let truth = match self.mysql_canonical(reference_sql).await {
            Ok(r) => r,
            Err(e) => {
                return Verdict::Error {
                    detail: format!("mysql: {e}"),
                }
            }
        };
        let cand = match self.pg_canonical(candidate_sql).await {
            Ok(r) => r,
            Err(e) => {
                return Verdict::Error {
                    detail: format!("pg: {e}"),
                }
            }
        };
        let ordered = is_ordered(candidate_sql) && is_ordered(reference_sql);
        if rows_equivalent(&cand, &truth, ordered) {
            Verdict::Equivalent
        } else {
            Verdict::Divergent {
                detail: format!("{} pg rows vs {} mysql rows", cand.len(), truth.len()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mysql_argv_from_env_absent_is_none() {
        // No DB needed: just the env parsing contract (var is normally unset).
        std::env::remove_var("PG_RETEST_MYSQL_CMD");
        assert!(LiveDiffOracle::mysql_argv_from_env().is_none());
    }
}
