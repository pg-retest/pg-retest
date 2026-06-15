//! LLM candidate generator: translate MySQL→PostgreSQL via an OpenAI-compatible chat
//! endpoint. Nondeterministic and external — but SAFE in this engine because the oracle
//! verifies every candidate behaviorally (a wrong LLM translation is rejected, never
//! trusted). This is exactly what the "slow is fine, offline" budget unlocks: spend an
//! LLM call to translate what the deterministic tools skip, and let the oracle referee.
//!
//! Config via env (absent `PG_RETEST_LLM_URL` ⇒ generator disabled):
//!   PG_RETEST_LLM_URL    chat-completions endpoint
//!                        (e.g. http://localhost:11434/v1/chat/completions for Ollama)
//!   PG_RETEST_LLM_MODEL  model name (default "gpt-4o-mini")
//!   PG_RETEST_LLM_KEY    bearer token (optional, for hosted providers)

use async_trait::async_trait;
use serde_json::json;

use super::engine::CandidateGenerator;

const SYSTEM_PROMPT: &str = "You are a SQL dialect translator. Translate the user's MySQL \
statement to a single equivalent PostgreSQL statement. Output ONLY the SQL — no prose, no \
explanation, no markdown code fences.";

pub struct LlmGenerator {
    client: reqwest::Client,
    url: String,
    model: String,
    api_key: Option<String>,
}

impl LlmGenerator {
    pub fn new(url: impl Into<String>, model: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            url: url.into(),
            model: model.into(),
            api_key,
        }
    }

    /// Build from env; `None` if `PG_RETEST_LLM_URL` is unset (generator disabled — the
    /// engine simply runs without it).
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("PG_RETEST_LLM_URL").ok()?;
        let model = std::env::var("PG_RETEST_LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
        let api_key = std::env::var("PG_RETEST_LLM_KEY").ok();
        Some(Self::new(url, model, api_key))
    }

    async fn complete(&self, mysql_sql: &str) -> Option<String> {
        let body = json!({
            "model": self.model,
            "temperature": 0,
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": mysql_sql},
            ],
        });
        let mut req = self.client.post(&self.url).json(&body);
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: serde_json::Value = resp.json().await.ok()?;
        let content = v["choices"][0]["message"]["content"].as_str()?;
        Some(sanitize(content))
    }
}

/// Strip code fences / a trailing semicolon an LLM may add; return the bare SQL.
pub fn sanitize(s: &str) -> String {
    let mut t = s.trim();
    if let Some(rest) = t.strip_prefix("```sql") {
        t = rest;
    } else if let Some(rest) = t.strip_prefix("```") {
        t = rest;
    }
    if let Some(rest) = t.strip_suffix("```") {
        t = rest;
    }
    t.trim().trim_end_matches(';').trim().to_string()
}

#[async_trait]
impl CandidateGenerator for LlmGenerator {
    fn name(&self) -> &'static str {
        "llm"
    }
    async fn candidate(&self, mysql_sql: &str) -> Option<String> {
        self.complete(mysql_sql).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Json, Router};
    use serde_json::Value;

    #[test]
    fn test_sanitize_strips_fences_and_trailing_semicolon() {
        assert_eq!(sanitize("```sql\nSELECT 1;\n```"), "SELECT 1");
        assert_eq!(
            sanitize("  SELECT COALESCE(a,b) FROM t  "),
            "SELECT COALESCE(a,b) FROM t"
        );
        assert_eq!(sanitize("```\nSELECT 2\n```"), "SELECT 2");
    }

    /// Spin a mock OpenAI-compatible endpoint returning a canned translation, proving
    /// the generator's HTTP request/response path end-to-end without a real provider.
    async fn mock_llm(reply_sql: &'static str) -> String {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(_body): Json<Value>| async move {
                Json(json!({
                    "choices": [ { "message": { "role": "assistant", "content": reply_sql } } ]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/v1/chat/completions")
    }

    #[tokio::test]
    async fn test_llm_generator_parses_candidate_from_mock_endpoint() {
        let url = mock_llm("```sql\nSELECT COALESCE(name, 'x') FROM t;\n```").await;
        let generator = LlmGenerator::new(url, "mock", None);
        let cand = generator.candidate("SELECT IFNULL(name,'x') FROM t").await;
        assert_eq!(cand.as_deref(), Some("SELECT COALESCE(name, 'x') FROM t"));
    }

    #[tokio::test]
    async fn test_llm_generator_declines_on_unreachable_endpoint() {
        // No server here → request fails → generator declines (None), never panics.
        let generator = LlmGenerator::new("http://127.0.0.1:1/v1/chat/completions", "x", None);
        assert!(generator.candidate("SELECT 1").await.is_none());
    }
}
