//! sqlglot candidate generator: translate MySQL→PostgreSQL via the mature Python
//! `sqlglot` transpiler, invoked as a subprocess (no Python Rust binding / no crate
//! dependency — the same external-tool pattern as the MySQL CLI). Oracle-verified like
//! every generator, so its output is trusted only when it behaves correctly.
//!
//! sqlglot is the upstream `polyglot-sql` derives from, and is more complete: e.g. it
//! translates MySQL `IF(c,a,b)` → `CASE WHEN c THEN a ELSE b END`, which the 0.5.4 Rust
//! port passes through (and PG then rejects at execution). Adding it as a generator lets
//! the verified-search engine pick it up exactly when it does better.
//!
//! Configure the interpreter via `PG_RETEST_SQLGLOT_PYTHON` (default `python3`); it must
//! be a Python that can `import sqlglot`. If it can't, the generator simply declines.

use std::process::Stdio;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::engine::CandidateGenerator;

/// The transpile script for a given source dialect: read SQL on stdin, print the single
/// PostgreSQL statement on stdout, or exit non-zero (multi-statement / parse error /
/// sqlglot missing). `read` is the sqlglot source dialect, e.g. "mysql" or "oracle".
fn script(read: &str) -> String {
    format!(
        r#"
import sys
try:
    import sqlglot
except Exception:
    sys.exit(10)
sql = sys.stdin.read()
try:
    out = sqlglot.transpile(sql, read="{read}", write="postgres")
except Exception:
    sys.exit(2)
if len(out) != 1:
    sys.exit(3)
sys.stdout.write(out[0])
"#
    )
}

pub struct SqlglotGenerator {
    python: String,
    /// sqlglot source dialect ("mysql", "oracle", …).
    read: String,
}

impl SqlglotGenerator {
    pub fn new(python: impl Into<String>, read: impl Into<String>) -> Self {
        Self {
            python: python.into(),
            read: read.into(),
        }
    }

    fn env_python() -> String {
        std::env::var("PG_RETEST_SQLGLOT_PYTHON").unwrap_or_else(|_| "python3".into())
    }

    /// MySQL→PG generator from `PG_RETEST_SQLGLOT_PYTHON` (default `python3`).
    pub fn from_env() -> Self {
        Self::new(Self::env_python(), "mysql")
    }

    /// Generator for an arbitrary sqlglot source dialect (e.g. "oracle").
    pub fn for_dialect(read: impl Into<String>) -> Self {
        Self::new(Self::env_python(), read)
    }

    /// True if `<python> -c "import sqlglot"` succeeds — gate tests/benchmarks on this.
    pub async fn available(&self) -> bool {
        Command::new(&self.python)
            .arg("-c")
            .arg("import sqlglot")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

#[async_trait]
impl CandidateGenerator for SqlglotGenerator {
    fn name(&self) -> &'static str {
        "sqlglot"
    }

    async fn candidate(&self, source_sql: &str) -> Option<String> {
        let mut child = Command::new(&self.python)
            .arg("-c")
            .arg(script(&self.read))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        {
            let mut stdin = child.stdin.take()?;
            stdin.write_all(source_sql.as_bytes()).await.ok()?;
            // stdin dropped here → EOF so the script's read() returns.
        }
        let out = child.wait_with_output().await.ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_oracle_dialect_translates_nvl_to_coalesce() {
        // Skips unless a real sqlglot is available (PG_RETEST_SQLGLOT_PYTHON).
        let g = SqlglotGenerator::for_dialect("oracle");
        if !g.available().await {
            eprintln!("SKIP: sqlglot not available");
            return;
        }
        let out = g.candidate("SELECT NVL(name, 'x') FROM t").await;
        assert!(
            out.as_deref().is_some_and(|s| s.contains("COALESCE")),
            "expected NVL→COALESCE, got: {out:?}"
        );
    }

    #[tokio::test]
    async fn test_declines_when_python_missing() {
        // A bogus interpreter → spawn fails → declines (None), never panics.
        let g = SqlglotGenerator::new("definitely-not-a-real-python-xyz", "mysql");
        assert!(!g.available().await);
        assert!(g.candidate("SELECT 1").await.is_none());
    }
}
