# M5: AI-Assisted Tuning Design

## Overview
Use AI (Claude API) to recommend PostgreSQL config, schema, and query changes, then automatically test iterations and produce comparison reports.

## Architecture

### LLM Integration
Direct HTTP calls to Claude API via `reqwest`:
```rust
pub struct ClaudeClient {
    api_key: String,
    model: String,  // "claude-sonnet-4-20250514"
    http: reqwest::Client,
}
```

No SDK dependency — simple POST to `/v1/messages` with tool use for structured output.

### Context Collection
Connect to target PG and extract:
- `pg_settings` — Current configuration
- `pg_stat_statements` — Query performance stats
- Schema definitions (tables, indexes, constraints)
- Index usage stats (`pg_stat_user_indexes`)
- Table stats (`pg_stat_user_tables`)
- EXPLAIN plans for top-N slowest queries from workload

### Recommendation Types

1. **Config changes** — `postgresql.conf` parameter adjustments
   - `shared_buffers`, `work_mem`, `effective_cache_size`, etc.
   - Output: List of `SET` statements

2. **Index creation** — Missing indexes for slow queries
   - Based on EXPLAIN plans and access patterns
   - Output: `CREATE INDEX` statements

3. **Query rewrites** — Optimized SQL for slow queries
   - Based on EXPLAIN analysis + schema knowledge
   - Output: Before/after SQL pairs

4. **Schema changes** — Partitioning, column types, constraints
   - Output: `ALTER TABLE` statements

### Tuning Loop
```
collect_context() -> LLM recommendations -> apply() -> replay() -> compare() -> repeat
```

- Max N iterations (default: 3)
- Each iteration produces a comparison report
- Stop early if no improvement or regressions

### A/B Variant Mode
```toml
[[variants]]
name = "baseline"
config = {}

[[variants]]
name = "high_memory"
config = { shared_buffers = "4GB", work_mem = "256MB" }

[[variants]]
name = "aggressive_indexes"
apply_sql = "indexes.sql"
```

Each variant gets:
- Isolated database instance (via provisioner from M3)
- Full replay
- Comparison report
- Side-by-side summary

### Safety Measures

- **Config parameter allowlist:** Only allow known-safe parameters
  - Block: `data_directory`, `hba_file`, `listen_addresses`, etc.
- **Dry-run by default:** Print recommendations without applying
- **Rollback tracking:** Record all changes for undo
- **Production hostname warning:** Refuse to modify databases with production-looking hostnames unless `--force` flag
- **Change confirmation:** Interactive prompt before each change (using `dialoguer`)

### New Command
```
pg-retest tune \
  --workload captured.wkl \
  --target "postgresql://localhost/testdb" \
  --api-key $ANTHROPIC_API_KEY \
  --max-iterations 3 \
  --dry-run
```

### New Modules
- `src/tuner/mod.rs` — Tuning orchestrator
- `src/tuner/llm.rs` — Claude API client
- `src/tuner/context.rs` — PG context collector
- `src/tuner/recommendation.rs` — Recommendation types + application
- `src/tuner/safety.rs` — Safety checks + allowlists
- `src/tuner/report.rs` — Tuning iteration reports
- `src/tuner/variants.rs` — A/B variant management

### New Dependencies
- `reqwest` (with `json` feature) — HTTP client for Claude API
- `dialoguer` (optional) — Interactive confirmation prompts

### Implementation Tasks
1. Claude API client (tool use for structured output)
2. PG context collector
3. Recommendation type system
4. Safety module (allowlist, dry-run, rollback)
5. Tuning loop orchestrator
6. A/B variant mode
7. Tuning report generation
8. CLI integration (`tune` subcommand)
9. Integration tests with mock LLM responses
