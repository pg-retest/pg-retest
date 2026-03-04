# M3: CI/CD Integration Design

## Overview
Automate the capture/replay/compare cycle as a pipeline step with pass/fail thresholds.

## Architecture

### Config Format: TOML
Use `.pg-retest.toml` as the pipeline configuration file. TOML is native to the Rust ecosystem and serde-compatible.

Example config:
```toml
[capture]
source_log = "pg_log.csv"
source_host = "prod-db-01"
pg_version = "16.2"
mask_values = true

[provision]
backend = "docker"
image = "postgres:16.2"
restore_from = "backup.sql"

[replay]
speed = 1.0
read_only = false
scale = 1

[thresholds]
p95_max_ms = 50.0
p99_max_ms = 200.0
error_rate_max_pct = 1.0
regression_max_count = 5
regression_threshold_pct = 20.0

[output]
json_report = "report.json"
junit_xml = "results.xml"
```

### New Command
`pg-retest run --config .pg-retest.toml` — full pipeline orchestrator that:
1. Reads config
2. Provisions target database (Docker)
3. Restores backup
4. Runs capture (if source_log provided)
5. Runs replay
6. Runs comparison
7. Evaluates thresholds
8. Outputs reports (terminal, JSON, JUnit XML)
9. Returns appropriate exit code

### Provisioning
Trait-based design:
```rust
pub trait Provisioner {
    async fn provision(&self, config: &ProvisionConfig) -> Result<ProvisionedDb>;
    async fn teardown(&self, db: &ProvisionedDb) -> Result<()>;
}
```
First backend: Docker via `bollard` crate or `docker` CLI subprocess.
`ProvisionedDb` contains connection string, container ID, cleanup handle.

### Exit Codes
- 0: Pass (all thresholds met)
- 1: Threshold violation (regressions, latency, errors exceeded)
- 2: Config error (invalid TOML, missing fields)
- 3: Capture error
- 4: Provision error (Docker not available, image pull failed)
- 5: Replay error (connection failed, timeout)

### JUnit XML Output
For CI test result integration (GitHub Actions, GitLab CI, Jenkins):
```xml
<testsuites>
  <testsuite name="pg-retest" tests="4" failures="1">
    <testcase name="p95_latency" time="0.050"/>
    <testcase name="p99_latency" time="0.200"/>
    <testcase name="error_rate" time="0.001"/>
    <testcase name="regression_count" time="0.000">
      <failure message="7 regressions found, max allowed: 5"/>
    </testcase>
  </testsuite>
</testsuites>
```

### CI Examples

**GitHub Actions:**
```yaml
- name: Database Performance Test
  run: |
    pg-retest run --config .pg-retest.toml
  env:
    DOCKER_HOST: unix:///var/run/docker.sock
```

**GitLab CI:**
```yaml
db-perf-test:
  stage: test
  services:
    - docker:dind
  script:
    - pg-retest run --config .pg-retest.toml
  artifacts:
    reports:
      junit: results.xml
```

### New Modules
- `src/config/mod.rs` — TOML config parsing + validation
- `src/pipeline/mod.rs` — Pipeline orchestrator
- `src/provision/mod.rs` — Provisioner trait + Docker backend

### New Dependencies
- `toml` — Config file parsing
- `bollard` (optional) — Docker API client
- `quick-xml` or manual — JUnit XML generation

### Implementation Tasks
1. Config module: TOML parsing with serde
2. Provisioner trait + Docker backend
3. Pipeline orchestrator
4. JUnit XML output
5. `run` CLI subcommand
6. Integration tests with Docker
