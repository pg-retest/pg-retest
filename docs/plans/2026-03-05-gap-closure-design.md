# Gap Closure Design: Per-Category Scaling, A/B Variant Testing, Cloud-Native Capture

**Date:** 2026-03-05
**Status:** Approved

## Overview

Three features to close gaps identified in the PRFAQ gap analysis:
1. **Per-Category Scaling** — Scale workload classes independently (M2 gap)
2. **A/B Variant Testing** — Compare replay results across different database targets
3. **Cloud-Native Capture (AWS RDS)** — Capture workload directly from RDS/Aurora without manual log download

---

## Feature 1: Per-Category Scaling

### Problem

The PRFAQ states users should be able to "classify captured workload into common workload categories" and "scale up each type of workload to simulate heavier workloads." Currently, `--scale N` duplicates all sessions uniformly. There is no way to scale analytical sessions 2x while scaling transactional sessions 4x.

### Design

Add per-class scale factors as CLI flags and TOML config fields. When any per-class flag is set, per-class scaling replaces uniform scaling.

**CLI flags** (on `replay` subcommand):
```
--scale-analytical N    Scale analytical sessions by N (default: 1)
--scale-transactional N Scale transactional sessions by N (default: 1)
--scale-mixed N         Scale mixed sessions by N (default: 1)
--scale-bulk N          Scale bulk sessions by N (default: 1)
```

**TOML config** (in `[replay]` section):
```toml
[replay]
scale_analytical = 2
scale_transactional = 4
scale_mixed = 1
scale_bulk = 0          # 0 = exclude entirely
stagger_ms = 500
```

**Behavior:**
- Mutually exclusive with `--scale N`. If any `--scale-*` class flag is provided, uniform `--scale` is ignored.
- Sessions are classified using existing `classify_session()` from `src/classify/mod.rs`.
- Scale factor of 0 excludes that class entirely (useful for isolating one workload type).
- Scale factor of 1 keeps original sessions (no duplication).
- Stagger applies across all scaled copies as before.
- A classification summary is printed before replay showing how many sessions are in each class and their scale factors.

**New code:**
- `src/replay/scaling.rs` — `scale_sessions_by_class()` function
- `src/cli.rs` — 4 new fields on `ReplayArgs`
- `src/config/mod.rs` — 4 new fields on `ReplayConfig` with `#[serde(default)]`
- `src/main.rs` — dispatch to per-class scaling when class flags are set
- `src/pipeline/mod.rs` — same dispatch in pipeline

**Algorithm:**
```
1. Classify all sessions: classify_session(session) for each
2. Group sessions by WorkloadClass
3. For each class with scale > 1: duplicate sessions (same as scale_sessions but per-group)
4. For each class with scale = 0: exclude sessions
5. Merge all groups, sort by start offset
6. Apply stagger offsets across all copies
```

---

## Feature 2: A/B Variant Testing

### Problem

Users need to compare performance across different database configurations, versions, or hosting environments. The current compare command only compares captured (source) latencies against a single replay run. There is no way to replay the same workload against two different targets and see which performed better.

### Design

New `pg-retest ab` subcommand that replays a workload against 2+ targets sequentially and produces a side-by-side comparison report.

**CLI:**
```
pg-retest ab \
  --workload captured.wkl \
  --variant "pg16-default=host=db1 dbname=app" \
  --variant "pg16-tuned=host=db2 dbname=app" \
  --read-only \
  --speed 1.0 \
  --json ab_report.json
```

**TOML config** (used with `pg-retest run` when `[[variants]]` is present):
```toml
[capture]
workload = "captured.wkl"

[[variants]]
label = "pg16-default"
target = "host=db1 dbname=app"

[[variants]]
label = "pg16-tuned"
target = "host=db2 dbname=app"

[replay]
read_only = true
speed = 1.0

[thresholds]
p95_max_ms = 50.0

[output]
json_report = "ab_report.json"
```

**Data flow:**
1. Load workload profile once
2. For each variant (sequentially, to avoid cross-contamination):
   a. Connect to variant's target
   b. Run replay with shared settings (speed, read_only, scale)
   c. Collect `Vec<ReplayResults>`
3. Compute `ABComparisonReport`:
   - First variant = baseline
   - For each subsequent variant, compute: avg/p50/p95/p99 latency, error count, regressions vs baseline
4. Print side-by-side terminal report
5. Write JSON report if requested
6. If thresholds are configured, evaluate against the best-performing variant

**New types:**

```rust
pub struct VariantResult {
    pub label: String,
    pub results: Vec<ReplayResults>,
    pub avg_latency_us: u64,
    pub p50_latency_us: u64,
    pub p95_latency_us: u64,
    pub p99_latency_us: u64,
    pub total_errors: u64,
    pub total_queries: u64,
}

pub struct ABComparisonReport {
    pub variants: Vec<VariantResult>,
    pub baseline_label: String,
    pub regressions: Vec<ABRegression>,  // queries slower in variant B vs A
    pub improvements: Vec<ABRegression>, // queries faster in variant B vs A
}

pub struct ABRegression {
    pub sql: String,
    pub baseline_us: u64,
    pub variant_label: String,
    pub variant_us: u64,
    pub change_pct: f64,
}
```

**Terminal report format:**
```
  A/B Comparison Report
  =====================

  Variant              Queries  Errors  Avg(ms)  P50(ms)  P95(ms)  P99(ms)
  pg16-default (base)    1,234       0    1.23    0.89     5.67    12.34
  pg16-tuned             1,234       0    0.98    0.72     4.12     9.87

  Winner: pg16-tuned (20% faster avg, 27% faster P95)

  Top Improvements (pg16-tuned vs pg16-default):
    SELECT ... FROM orders WHERE ...   5.67ms → 4.12ms (-27.4%)
    ...

  Regressions (pg16-tuned vs pg16-default):
    (none)
```

**New code:**
- `src/compare/ab.rs` — `ABComparisonReport`, `compute_ab_comparison()`, terminal/JSON report
- `src/cli.rs` — new `AB(ABArgs)` variant in `Commands` enum
- `src/main.rs` — `cmd_ab()` function
- `src/config/mod.rs` — `VariantConfig` struct, `variants: Option<Vec<VariantConfig>>` on `PipelineConfig`
- `src/pipeline/mod.rs` — detect `[[variants]]` and switch to A/B mode

**Key decisions:**
- Variants replay sequentially (not in parallel) to avoid interference
- First variant is always the baseline
- Regressions are detected against the baseline using the same threshold_pct as regular compare
- A/B mode and normal mode are mutually exclusive in pipeline config (presence of `[[variants]]` triggers A/B)

---

## Feature 3: Cloud-Native Capture (AWS RDS/Aurora)

### Problem

Users with RDS/Aurora must manually download PG CSV logs from the AWS console or CLI before running `pg-retest capture`. This is tedious and error-prone, especially for automated pipelines.

### Design

New capture backend that wraps the AWS CLI to download RDS log files, then delegates to the existing `CsvLogCapture` parser.

**CLI:**
```
pg-retest capture \
  --source-type rds \
  --rds-instance mydb-instance \
  --rds-region us-east-1 \
  --rds-log-file postgresql.log.2024-03-08-10 \
  -o workload.wkl
```

If `--rds-log-file` is omitted, the tool lists available log files and downloads the most recent one.

**TOML config:**
```toml
[capture]
source_type = "rds"
rds_instance = "mydb-instance"
rds_region = "us-east-1"
# rds_log_file = "postgresql.log.2024-03-08-10"  # optional, defaults to latest
```

**Prerequisites:**
- `aws` CLI installed and configured (env vars, profile, or IAM role)
- RDS instance configured with:
  - `log_destination = 'csvlog'` (or `stderr` with `csvlog` format)
  - `log_statement = 'all'` (or at minimum `'mod'` for DML)
  - `log_duration = on` (for timing data)

**Data flow:**
1. Validate `aws` CLI is available (`aws --version`)
2. If no log file specified: call `aws rds describe-db-log-files --db-instance-identifier <id>` to list files, pick the most recent
3. Download log file: `aws rds download-db-log-file-portion --db-instance-identifier <id> --log-file-name <name> --output text`
   - Handle pagination: the API returns max 1MB per call with a `Marker` for continuation
   - Write all pages to a temp file
4. Pass temp file to `CsvLogCapture::capture_from_file()`
5. Clean up temp file
6. Return `WorkloadProfile` with `capture_method = "rds"`

**New code:**
- `src/capture/rds.rs` — `RdsCapture` struct with `capture_from_instance()` method
- `src/capture/mod.rs` — add `pub mod rds;`
- `src/cli.rs` — add `rds_instance`, `rds_region`, `rds_log_file` to `CaptureArgs`
- `src/config/mod.rs` — add RDS fields to `CaptureConfig`
- `src/main.rs` + `src/pipeline/mod.rs` — dispatch `"rds"` source type

**Error handling:**
- `aws` CLI not found: clear error message with install instructions
- No log files available: suggest checking RDS logging configuration
- Download failure: retry once, then fail with the AWS error message
- CSV parse failure: suggest checking `log_destination = 'csvlog'` setting

**Pagination:**
RDS `download-db-log-file-portion` returns at most 1MB per call. The response includes:
- `LogFileData` — the log content
- `Marker` — position for the next call
- `AdditionalDataPending` — boolean indicating more data

The download loop calls repeatedly with `--starting-token <Marker>` until `AdditionalDataPending` is false.

---

## Build Order

These three features are independent and can be built in any order. Recommended sequence:

1. **Per-Category Scaling** — smallest scope, builds on existing classify + scale infrastructure
2. **A/B Variant Testing** — medium scope, new subcommand and comparison logic
3. **Cloud-Native Capture** — largest scope, external dependency on AWS CLI, pagination handling

---

## Testing Strategy

**Per-Category Scaling:**
- Unit tests: `scale_sessions_by_class()` with known classification inputs
- Integration test: classify + scale + verify session counts per class
- Edge cases: all sessions same class, scale=0 exclusion, empty classes

**A/B Variant Testing:**
- Unit tests: `compute_ab_comparison()` with mock `VariantResult` data
- Integration test: A/B with two different (non-existent) targets — verify it gets past config parsing
- Report formatting tests

**Cloud-Native Capture (RDS):**
- Unit tests: pagination assembly logic, log file selection
- Integration test with mock `aws` CLI (shell script that returns fixture data)
- Error handling tests: missing CLI, no log files, download failure
