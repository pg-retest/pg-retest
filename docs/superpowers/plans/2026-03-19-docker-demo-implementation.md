# Docker Demo Environment & README Use Cases Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add README hero use cases, Docker demo environment with e-commerce databases, web dashboard demo page with guided wizard and scenario cards, and missing docs for AI tuning and transform features.

**Architecture:** Three layers — (1) README content update, (2) Docker infrastructure (Dockerfile, compose, SQL seeds, demo workload generator), (3) Rust backend + Alpine.js frontend for the demo page. The demo page handlers orchestrate existing replay/compare/tune functions with pre-configured parameters. Demo mode is activated by `PG_RETEST_DEMO=true` env var.

**Tech Stack:** Rust/Axum (backend), Alpine.js/Tailwind/Chart.js (frontend), Docker/docker-compose, PostgreSQL 16, shell scripts for workload generation.

**Spec:** `docs/plans/2026-03-19-docker-demo-design.md`

---

### Task 1: README Hero Use Cases

**Files:**
- Modify: `README.md:10` (insert after tagline, before Quick Start)

- [ ] **Step 1: Add the hero use cases section to README**

Insert immediately after line 10 (`Capture, replay, and compare PostgreSQL workloads...`) and before the mermaid diagram (line 12):

```markdown
---

## Why pg-retest?

| | |
|---|---|
| **🔄 Pre-Migration Validation** | Replay production traffic against your new datacenter, hardware, or cloud target before cutting over. Know it works — don't hope. |
| **⬆️ Version & Patch Testing** | Upgrading PostgreSQL 15 → 16? Replay your exact workload against the new version and catch regressions before they hit production. |
| **⚙️ Configuration Benchmarking** | Changed `shared_buffers` or `work_mem`? Compare before and after with real queries, not synthetic benchmarks. |
| **☁️ Cloud Provider Evaluation** | RDS vs. Aurora vs. AlloyDB vs. self-hosted — replay identical traffic against each and let the numbers decide. |
| **📈 Capacity Planning** | Scale your workload 2x, 5x, 10x to find where things break — before Black Friday finds it for you. |
| **🚦 CI/CD Regression Gates** | Automated pass/fail on every schema migration or config change. Catch performance regressions in the pipeline, not in production. |
| **🔀 Cross-Database Migration** | Moving from MySQL to PostgreSQL? Capture your MySQL workload, transform the SQL, and validate it runs correctly on PG. |
| **🤖 AI-Assisted Optimization** | Get LLM-powered tuning recommendations — then validate every change against your real workload with automatic rollback on regression. |
```

- [ ] **Step 2: Verify the README renders correctly**

Run: `head -40 README.md`
Expected: Hero section appears between tagline and mermaid diagram.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add hero use cases section to README"
```

---

### Task 2: Docker Infrastructure — Dockerfile and docker-compose.yml

**Files:**
- Create: `Dockerfile`
- Create: `docker-compose.yml`
- Create: `.dockerignore`

- [ ] **Step 1: Create .dockerignore**

```
target/
.git/
*.wkl
data/
.openai/
```

- [ ] **Step 2: Create Dockerfile**

```dockerfile
# Stage 1: Build
FROM rust:1.75-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --release

# Stage 2: Runtime
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends libssl3 ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/pg-retest /usr/local/bin/pg-retest
RUN mkdir -p /data/workloads
EXPOSE 8080
ENTRYPOINT ["pg-retest"]
CMD ["web", "--port", "8080", "--data-dir", "/data"]
```

- [ ] **Step 3: Create docker-compose.yml**

```yaml
services:
  db-a:
    image: postgres:16
    environment:
      POSTGRES_DB: ecommerce
      POSTGRES_USER: demo
      POSTGRES_PASSWORD: demo
    volumes:
      - ./demo/init-db-a.sql:/docker-entrypoint-initdb.d/init.sql
    ports:
      - "5450:5432"
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U demo -d ecommerce"]
      interval: 5s
      timeout: 5s
      retries: 5

  db-b:
    image: postgres:16
    environment:
      POSTGRES_DB: ecommerce
      POSTGRES_USER: demo
      POSTGRES_PASSWORD: demo
    volumes:
      - ./demo/init-db-b.sql:/docker-entrypoint-initdb.d/init.sql
    ports:
      - "5451:5432"
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U demo -d ecommerce"]
      interval: 5s
      timeout: 5s
      retries: 5

  pg-retest:
    build: .
    environment:
      PG_RETEST_DEMO: "true"
      DEMO_DB_A: "host=db-a dbname=ecommerce user=demo password=demo"
      DEMO_DB_B: "host=db-b dbname=ecommerce user=demo password=demo"
      DEMO_WORKLOAD: "/demo/workload.wkl"
    ports:
      - "8080:8080"
    volumes:
      - ./demo:/demo:ro
      - pgdata:/data
    depends_on:
      db-a:
        condition: service_healthy
      db-b:
        condition: service_healthy

volumes:
  pgdata:
```

- [ ] **Step 4: Verify Dockerfile syntax**

Run: `docker build --check . 2>&1 || echo "Docker not available, manual review needed"`

- [ ] **Step 5: Commit**

```bash
git add Dockerfile docker-compose.yml .dockerignore
git commit -m "feat(docker): add Dockerfile and docker-compose for demo environment"
```

---

### Task 3: E-Commerce Database Seeds

**Files:**
- Create: `demo/init-db-a.sql`
- Create: `demo/init-db-b.sql`

- [ ] **Step 1: Create demo/init-db-a.sql with schema and seed data**

This file must contain:
1. Schema creation (5 tables: customers, products, orders, order_items, reviews)
2. Indexes on all foreign keys + composite indexes
3. Seed data generation using `generate_series()`:
   - ~5,000 customers with names/emails
   - ~1,000 products across 10 categories with prices and stock
   - ~20,000 orders linked to customers with statuses (pending/shipped/delivered/cancelled)
   - ~60,000 order_items linked to orders and products
   - ~8,000 reviews with ratings 1-5

```sql
-- E-Commerce Demo Schema + Seed Data for pg-retest

-- Schema
CREATE TABLE customers (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE products (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    category TEXT NOT NULL,
    price NUMERIC(10,2) NOT NULL,
    stock INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE orders (
    id SERIAL PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers(id),
    total NUMERIC(10,2) NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE order_items (
    id SERIAL PRIMARY KEY,
    order_id INTEGER NOT NULL REFERENCES orders(id),
    product_id INTEGER NOT NULL REFERENCES products(id),
    qty INTEGER NOT NULL DEFAULT 1,
    price NUMERIC(10,2) NOT NULL
);

CREATE TABLE reviews (
    id SERIAL PRIMARY KEY,
    product_id INTEGER NOT NULL REFERENCES products(id),
    customer_id INTEGER NOT NULL REFERENCES customers(id),
    rating INTEGER NOT NULL CHECK (rating BETWEEN 1 AND 5),
    body TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Indexes
CREATE INDEX idx_orders_customer_id ON orders(customer_id);
CREATE INDEX idx_orders_status_created ON orders(status, created_at);
CREATE INDEX idx_orders_customer_created ON orders(customer_id, created_at);
CREATE INDEX idx_order_items_order_id ON order_items(order_id);
CREATE INDEX idx_order_items_product_id ON order_items(product_id);
CREATE INDEX idx_order_items_product_qty ON order_items(product_id, qty);
CREATE INDEX idx_reviews_product_id ON reviews(product_id);
CREATE INDEX idx_reviews_product_rating ON reviews(product_id, rating);
CREATE INDEX idx_reviews_customer_id ON reviews(customer_id);

-- Seed data: Customers (~5,000)
INSERT INTO customers (name, email, created_at)
SELECT
    'Customer ' || i,
    'customer' || i || '@example.com',
    now() - (random() * interval '365 days')
FROM generate_series(1, 5000) AS i;

-- Seed data: Products (~1,000)
INSERT INTO products (name, category, price, stock)
SELECT
    'Product ' || i,
    (ARRAY['Electronics', 'Clothing', 'Books', 'Home', 'Sports',
           'Toys', 'Food', 'Beauty', 'Garden', 'Auto'])[1 + (i % 10)],
    round((random() * 200 + 5)::numeric, 2),
    (random() * 500)::integer
FROM generate_series(1, 1000) AS i;

-- Seed data: Orders (~20,000)
INSERT INTO orders (customer_id, total, status, created_at)
SELECT
    1 + (random() * 4999)::integer,
    round((random() * 500 + 10)::numeric, 2),
    (ARRAY['pending', 'shipped', 'delivered', 'cancelled'])[1 + (i % 4)],
    now() - (random() * interval '180 days')
FROM generate_series(1, 20000) AS i;

-- Seed data: Order Items (~60,000, ~3 per order)
INSERT INTO order_items (order_id, product_id, qty, price)
SELECT
    1 + (i / 3),
    1 + (random() * 999)::integer,
    1 + (random() * 4)::integer,
    round((random() * 100 + 5)::numeric, 2)
FROM generate_series(0, 59999) AS i;

-- Seed data: Reviews (~8,000)
INSERT INTO reviews (product_id, customer_id, rating, body, created_at)
SELECT
    1 + (random() * 999)::integer,
    1 + (random() * 4999)::integer,
    1 + (random() * 4)::integer,
    CASE (i % 5)
        WHEN 0 THEN 'Great product, highly recommend!'
        WHEN 1 THEN 'Good value for the price.'
        WHEN 2 THEN 'Average quality, nothing special.'
        WHEN 3 THEN 'Below expectations, would not buy again.'
        WHEN 4 THEN 'Excellent! Exceeded all expectations.'
    END,
    now() - (random() * interval '180 days')
FROM generate_series(1, 8000) AS i;

-- Analyze tables for query planner
ANALYZE;
```

- [ ] **Step 2: Create demo/init-db-b.sql (same schema + data)**

Copy `init-db-a.sql` to `init-db-b.sql`. Both databases need identical data for meaningful replay comparison.

```bash
cp demo/init-db-a.sql demo/init-db-b.sql
```

- [ ] **Step 3: Test seed SQL against a local PostgreSQL**

Run: `docker run --rm -e POSTGRES_DB=test -e POSTGRES_USER=demo -e POSTGRES_PASSWORD=demo -v $(pwd)/demo/init-db-a.sql:/docker-entrypoint-initdb.d/init.sql postgres:16 postgres -c 'max_connections=10'`
Expected: Container starts, seed completes without errors, then stop with Ctrl+C.

- [ ] **Step 4: Commit**

```bash
git add demo/init-db-a.sql demo/init-db-b.sql
git commit -m "feat(demo): add e-commerce schema and seed data for demo databases"
```

---

### Task 4: Demo Workload Generator Script

**Files:**
- Create: `demo/generate-workload.sh`
- Create: `demo/sessions/analytical-1.sql`
- Create: `demo/sessions/analytical-2.sql`
- Create: `demo/sessions/transactional-1.sql`
- Create: `demo/sessions/transactional-2.sql`
- Create: `demo/sessions/transactional-3.sql`
- Create: `demo/sessions/mixed-1.sql`
- Create: `demo/sessions/mixed-2.sql`
- Create: `demo/sessions/bulk-1.sql`

- [ ] **Step 1: Create session SQL scripts**

Each script runs through the proxy as one session. Scripts should include `\timing` and take ~2 minutes total across all sessions running concurrently.

**demo/sessions/analytical-1.sql** — Revenue analytics:
```sql
-- Analytical session 1: Revenue analytics
SELECT c.name, COUNT(o.id) as order_count, SUM(o.total) as total_spent
FROM customers c JOIN orders o ON c.id = o.customer_id
WHERE o.created_at > now() - interval '30 days'
GROUP BY c.id, c.name ORDER BY total_spent DESC LIMIT 20;

SELECT p.category, COUNT(DISTINCT oi.order_id) as orders, SUM(oi.qty * oi.price) as revenue
FROM products p JOIN order_items oi ON p.id = oi.product_id
JOIN orders o ON oi.order_id = o.id
WHERE o.created_at > now() - interval '90 days'
GROUP BY p.category ORDER BY revenue DESC;

SELECT date_trunc('day', o.created_at) as day, COUNT(*) as orders, SUM(o.total) as revenue
FROM orders o WHERE o.created_at > now() - interval '30 days'
GROUP BY day ORDER BY day;

SELECT p.name, AVG(r.rating) as avg_rating, COUNT(r.id) as review_count
FROM products p JOIN reviews r ON p.id = r.product_id
GROUP BY p.id, p.name HAVING COUNT(r.id) > 3 ORDER BY avg_rating DESC LIMIT 25;

-- Repeat with variations for ~2 min runtime
SELECT o.status, COUNT(*) as cnt, AVG(o.total) as avg_total
FROM orders o GROUP BY o.status;

SELECT p.category, p.name, p.stock, p.price
FROM products p WHERE p.stock < 20 ORDER BY p.stock ASC;

SELECT c.id, c.name, c.email, MAX(o.created_at) as last_order
FROM customers c LEFT JOIN orders o ON c.id = o.customer_id
GROUP BY c.id, c.name, c.email
HAVING MAX(o.created_at) < now() - interval '60 days' OR MAX(o.created_at) IS NULL
LIMIT 50;
```

**demo/sessions/analytical-2.sql** — Product analytics (similar pattern, different queries).

**demo/sessions/transactional-1.sql** — Order placement:
```sql
-- Transactional session 1: Place orders
BEGIN;
INSERT INTO orders (customer_id, total, status) VALUES (1 + (random()*4999)::int, 0, 'pending') RETURNING id;
-- Note: In the real script, use a fixed customer_id per transaction
COMMIT;

BEGIN;
INSERT INTO orders (customer_id, total, status, created_at) VALUES (42, 150.00, 'pending', now());
INSERT INTO order_items (order_id, product_id, qty, price) VALUES (currval('orders_id_seq'), 10, 2, 49.99);
INSERT INTO order_items (order_id, product_id, qty, price) VALUES (currval('orders_id_seq'), 25, 1, 50.02);
UPDATE orders SET total = 150.00 WHERE id = currval('orders_id_seq');
UPDATE products SET stock = stock - 2 WHERE id = 10;
UPDATE products SET stock = stock - 1 WHERE id = 25;
COMMIT;

-- Repeat similar transaction blocks with different customer/product IDs
-- 10-15 transactions total for ~2 min session
```

**demo/sessions/transactional-2.sql, transactional-3.sql** — Similar order placement patterns with different customer/product IDs.

**demo/sessions/mixed-1.sql** — Customer lookup + light writes:
```sql
-- Mixed session 1: Customer activity
SELECT * FROM customers WHERE id = 100;
SELECT o.id, o.total, o.status, o.created_at FROM orders o WHERE o.customer_id = 100 ORDER BY o.created_at DESC LIMIT 10;
SELECT oi.product_id, p.name, oi.qty, oi.price FROM order_items oi JOIN products p ON oi.product_id = p.id WHERE oi.order_id = 1;

BEGIN;
INSERT INTO reviews (product_id, customer_id, rating, body) VALUES (50, 100, 4, 'Good product');
COMMIT;

SELECT * FROM products WHERE id = 50;
SELECT AVG(rating) as avg_rating FROM reviews WHERE product_id = 50;

-- More lookup patterns for different customers
```

**demo/sessions/mixed-2.sql** — Similar pattern, different customers.

**demo/sessions/bulk-1.sql** — Batch status updates:
```sql
-- Bulk session: Batch order status updates
UPDATE orders SET status = 'shipped' WHERE status = 'pending' AND created_at < now() - interval '7 days' AND id % 10 = 0;
UPDATE orders SET status = 'delivered' WHERE status = 'shipped' AND created_at < now() - interval '14 days' AND id % 10 = 0;

SELECT pg_sleep(2);

UPDATE orders SET status = 'shipped' WHERE status = 'pending' AND created_at < now() - interval '7 days' AND id % 10 = 1;
UPDATE orders SET status = 'delivered' WHERE status = 'shipped' AND created_at < now() - interval '14 days' AND id % 10 = 1;
```

- [ ] **Step 2: Create demo/generate-workload.sh**

```bash
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "==> Starting temporary PostgreSQL container..."
docker run -d --name pg-retest-demo-gen \
    -e POSTGRES_DB=ecommerce \
    -e POSTGRES_USER=demo \
    -e POSTGRES_PASSWORD=demo \
    -p 5460:5432 \
    -v "$SCRIPT_DIR/init-db-a.sql:/docker-entrypoint-initdb.d/init.sql" \
    postgres:16

echo "==> Waiting for PostgreSQL to be ready..."
until docker exec pg-retest-demo-gen pg_isready -U demo -d ecommerce 2>/dev/null; do
    sleep 1
done
sleep 2  # Extra wait for init script to complete

echo "==> Building pg-retest..."
(cd "$PROJECT_DIR" && cargo build --release)
PGRETEST="$PROJECT_DIR/target/release/pg-retest"

echo "==> Starting proxy capture..."
$PGRETEST proxy \
    --listen 0.0.0.0:5461 \
    --target localhost:5460 \
    --output "$SCRIPT_DIR/workload.wkl" &
PROXY_PID=$!
sleep 2  # Wait for proxy to start

echo "==> Running workload sessions in parallel..."
CONNSTR="host=localhost port=5461 dbname=ecommerce user=demo password=demo"

for session_file in "$SCRIPT_DIR"/sessions/*.sql; do
    echo "    Starting session: $(basename "$session_file")"
    PGPASSWORD=demo psql -h localhost -p 5461 -U demo -d ecommerce -f "$session_file" > /dev/null 2>&1 &
done

echo "==> Waiting for all sessions to complete..."
wait

echo "==> Stopping proxy (generates workload.wkl)..."
kill $PROXY_PID 2>/dev/null || true
wait $PROXY_PID 2>/dev/null || true

echo "==> Cleaning up container..."
docker rm -f pg-retest-demo-gen

echo "==> Done! Demo workload saved to: $SCRIPT_DIR/workload.wkl"
$PGRETEST inspect "$SCRIPT_DIR/workload.wkl" --classify
```

- [ ] **Step 3: Make generate script executable**

```bash
chmod +x demo/generate-workload.sh
```

- [ ] **Step 4: Commit**

```bash
git add demo/generate-workload.sh demo/sessions/
git commit -m "feat(demo): add workload generation script and session SQL files"
```

---

### Task 5: Backend — DemoConfig in AppState

**Files:**
- Modify: `src/web/state.rs`
- Modify: `src/web/mod.rs`

- [ ] **Step 1: Write test for DemoConfig parsing from env vars**

Add to `src/web/state.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_demo_config_from_env_disabled() {
        // Clear any existing env vars
        std::env::remove_var("PG_RETEST_DEMO");
        let config = DemoConfig::from_env();
        assert!(config.is_none());
    }

    #[test]
    fn test_demo_config_from_env_enabled() {
        std::env::set_var("PG_RETEST_DEMO", "true");
        std::env::set_var("DEMO_DB_A", "host=db-a dbname=ecommerce user=demo password=demo");
        std::env::set_var("DEMO_DB_B", "host=db-b dbname=ecommerce user=demo password=demo");
        std::env::set_var("DEMO_WORKLOAD", "/demo/workload.wkl");
        let config = DemoConfig::from_env();
        assert!(config.is_some());
        let c = config.unwrap();
        assert!(c.db_a.contains("db-a"));
        assert!(c.db_b.contains("db-b"));
        assert_eq!(c.workload_path, PathBuf::from("/demo/workload.wkl"));
        // Clean up
        std::env::remove_var("PG_RETEST_DEMO");
        std::env::remove_var("DEMO_DB_A");
        std::env::remove_var("DEMO_DB_B");
        std::env::remove_var("DEMO_WORKLOAD");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib web::state::tests`
Expected: FAIL — `DemoConfig` not defined.

- [ ] **Step 3: Implement DemoConfig and add to AppState**

> **Note:** Steps 3 and 4 must be implemented together before building — changing `AppState::new()` signature without updating the call site will break compilation.

In `src/web/state.rs`, add:

```rust
/// Configuration for demo mode, parsed from environment variables.
#[derive(Clone, Debug)]
pub struct DemoConfig {
    pub db_a: String,
    pub db_b: String,
    pub workload_path: PathBuf,
    pub init_sql_path: PathBuf,
}

impl DemoConfig {
    /// Parse demo configuration from environment variables.
    /// Returns None if PG_RETEST_DEMO is not set to "true".
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("PG_RETEST_DEMO").unwrap_or_default();
        if enabled != "true" {
            return None;
        }
        let db_a = std::env::var("DEMO_DB_A").unwrap_or_default();
        let db_b = std::env::var("DEMO_DB_B").unwrap_or_default();
        let workload = std::env::var("DEMO_WORKLOAD").unwrap_or_else(|_| "/demo/workload.wkl".to_string());
        if db_a.is_empty() || db_b.is_empty() {
            return None;
        }
        Some(Self {
            db_a,
            db_b,
            workload_path: PathBuf::from(&workload),
            init_sql_path: PathBuf::from(workload).parent().unwrap_or(std::path::Path::new("/demo")).join("init-db-b.sql"),
        })
    }
}
```

Add `demo_config` field to `AppState`:

```rust
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub data_dir: PathBuf,
    pub ws_tx: broadcast::Sender<WsMessage>,
    pub tasks: Arc<TaskManager>,
    pub demo_config: Option<DemoConfig>,
}
```

Update `AppState::new()` to accept optional demo config:

```rust
pub fn new(db: Connection, data_dir: PathBuf, demo_config: Option<DemoConfig>) -> Self {
    let (ws_tx, _) = broadcast::channel(1024);
    Self {
        db: Arc::new(Mutex::new(db)),
        data_dir,
        ws_tx,
        tasks: Arc::new(TaskManager::new()),
        demo_config,
    }
}
```

- [ ] **Step 4: Update run_server to parse DemoConfig and pass to AppState**

In `src/web/mod.rs`, update `run_server`:

```rust
pub async fn run_server(port: u16, data_dir: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&data_dir)?;

    let db_path = data_dir.join("pg-retest.db");
    let conn = rusqlite::Connection::open(&db_path)?;
    db::init_db(&conn)?;

    let demo_config = state::DemoConfig::from_env();
    if let Some(ref dc) = demo_config {
        println!("Demo mode: enabled");
        println!("  Database A: {}", dc.db_a);
        println!("  Database B: {}", dc.db_b);
        println!("  Workload: {}", dc.workload_path.display());
    }

    let state = AppState::new(conn, data_dir.clone(), demo_config);

    // Auto-import demo workload if in demo mode
    if let Some(ref dc) = state.demo_config {
        if dc.workload_path.exists() {
            let db = state.db.lock().await;
            import_demo_workload(&db, dc, &data_dir).ok();
        }
    }

    let app = routes::build_router(state).fallback(static_handler);

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    println!("pg-retest web dashboard: http://localhost:{port}");
    println!("Data directory: {}", data_dir.display());

    axum::serve(listener, app).await?;
    Ok(())
}

/// Import the demo workload into SQLite so it's accessible via normal handlers.
fn import_demo_workload(conn: &rusqlite::Connection, dc: &state::DemoConfig, data_dir: &std::path::Path) -> Result<()> {
    use crate::profile::io::read_profile;

    // Check if already imported
    let existing = db::list_workloads(conn)?;
    if existing.iter().any(|w| w.name == "demo-ecommerce") {
        return Ok(());
    }

    let profile = read_profile(&dc.workload_path)?;
    let dest = data_dir.join("workloads").join("demo-ecommerce.wkl");
    if !dest.exists() {
        std::fs::create_dir_all(dest.parent().unwrap())?;
        std::fs::copy(&dc.workload_path, &dest)?;
    }

    let row = db::WorkloadRow {
        id: uuid::Uuid::new_v4().to_string(),
        name: "demo-ecommerce".to_string(),
        file_path: dest.to_string_lossy().to_string(),
        source_type: Some(if profile.capture_method.is_empty() { "demo".to_string() } else { profile.capture_method.clone() }),
        source_host: Some(profile.source_host.clone()),
        captured_at: Some(profile.captured_at.to_rfc3339()),
        total_sessions: Some(profile.sessions.len() as i64),
        total_queries: Some(profile.sessions.iter().map(|s| s.queries.len() as i64).sum()),
        capture_duration_us: None,
        classification: None,
        created_at: None,
    };
    db::insert_workload(conn, &row)?;
    Ok(())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib web::state::tests`
Expected: PASS

- [ ] **Step 6: Run full build**

Run: `cargo build`
Expected: No compilation errors.

- [ ] **Step 7: Commit**

```bash
git add src/web/state.rs src/web/mod.rs
git commit -m "feat(web): add DemoConfig to AppState with env var parsing and auto-import"
```

---

### Task 6: Backend — Demo API Handlers

**Files:**
- Create: `src/web/handlers/demo.rs`
- Modify: `src/web/handlers/mod.rs`
- Modify: `src/web/routes.rs`

- [ ] **Step 1: Create src/web/handlers/demo.rs with config endpoint**

```rust
use axum::{extract::State, http::StatusCode, Json};
use serde_json::json;

use crate::web::state::AppState;

/// GET /api/v1/demo/config
pub async fn get_config(State(state): State<AppState>) -> Result<Json<serde_json::Value>, StatusCode> {
    match &state.demo_config {
        Some(dc) => Ok(Json(json!({
            "enabled": true,
            "db_a_host": dc.db_a.split_whitespace()
                .find(|s| s.starts_with("host="))
                .map(|s| s.trim_start_matches("host="))
                .unwrap_or("db-a"),
            "db_b_host": dc.db_b.split_whitespace()
                .find(|s| s.starts_with("host="))
                .map(|s| s.trim_start_matches("host="))
                .unwrap_or("db-b"),
        }))),
        None => Ok(Json(json!({ "enabled": false }))),
    }
}

/// POST /api/v1/demo/reset-db — Reset Database B to initial state
pub async fn reset_db(State(state): State<AppState>) -> Result<Json<serde_json::Value>, StatusCode> {
    let dc = state.demo_config.as_ref().ok_or(StatusCode::NOT_FOUND)?;

    let init_sql = std::fs::read_to_string(&dc.init_sql_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Connect to DB-B and re-run init SQL
    let (client, connection) = tokio_postgres::connect(&dc.db_b, tokio_postgres::NoTls)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    tokio::spawn(async move { connection.await.ok(); });

    // Drop and recreate all tables
    client.batch_execute("DROP TABLE IF EXISTS reviews, order_items, orders, products, customers CASCADE;")
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    client.batch_execute(&init_sql)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({ "status": "reset_complete" })))
}
```

- [ ] **Step 2: Add wizard step and scenario handlers**

Add to `src/web/handlers/demo.rs`:

```rust
use axum::extract::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashMap;
use once_cell::sync::Lazy;

/// In-memory state for wizard steps and scenarios (reset on restart).
static DEMO_STATE: Lazy<Arc<RwLock<DemoState>>> = Lazy::new(|| {
    Arc::new(RwLock::new(DemoState::default()))
});

#[derive(Default)]
struct DemoState {
    wizard_results: HashMap<u32, serde_json::Value>,
    scenario_results: HashMap<String, serde_json::Value>,
    wizard_status: HashMap<u32, String>,
    scenario_status: HashMap<String, String>,
}

/// POST /api/v1/demo/wizard/:step
pub async fn run_wizard_step(
    State(state): State<AppState>,
    Path(step): Path<u32>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let dc = state.demo_config.as_ref().ok_or(StatusCode::NOT_FOUND)?;

    // Mark step as running
    {
        let mut ds = DEMO_STATE.write().await;
        ds.wizard_status.insert(step, "running".to_string());
    }

    let result = match step {
        1 => run_wizard_explore(&state, dc).await,
        2 => run_wizard_replay(&state, dc).await,
        3 => run_wizard_compare(&state, dc).await,
        4 => run_wizard_scale(&state, dc).await,
        5 => run_wizard_tune(&state, dc).await,
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    match result {
        Ok(value) => {
            let mut ds = DEMO_STATE.write().await;
            ds.wizard_status.insert(step, "completed".to_string());
            ds.wizard_results.insert(step, value.clone());
            Ok(Json(json!({ "status": "completed", "result": value })))
        }
        Err(e) => {
            let mut ds = DEMO_STATE.write().await;
            ds.wizard_status.insert(step, "error".to_string());
            Ok(Json(json!({ "status": "error", "error": e })))
        }
    }
}

/// GET /api/v1/demo/wizard/:step
pub async fn get_wizard_step(
    State(state): State<AppState>,
    Path(step): Path<u32>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if state.demo_config.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let ds = DEMO_STATE.read().await;
    let status = ds.wizard_status.get(&step).cloned().unwrap_or_else(|| "pending".to_string());
    let result = ds.wizard_results.get(&step).cloned();
    Ok(Json(json!({ "step": step, "status": status, "result": result })))
}

/// POST /api/v1/demo/scenario/:name
pub async fn run_scenario(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let dc = state.demo_config.as_ref().ok_or(StatusCode::NOT_FOUND)?;

    {
        let mut ds = DEMO_STATE.write().await;
        ds.scenario_status.insert(name.clone(), "running".to_string());
    }

    let result = match name.as_str() {
        "migration" => run_scenario_migration(&state, dc).await,
        "capacity" => run_scenario_capacity(&state, dc).await,
        "ab" => run_scenario_ab(&state, dc).await,
        "tuning" => run_scenario_tuning(&state, dc).await,
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    match result {
        Ok(value) => {
            let mut ds = DEMO_STATE.write().await;
            ds.scenario_status.insert(name.clone(), "completed".to_string());
            ds.scenario_results.insert(name, value.clone());
            Ok(Json(json!({ "status": "completed", "result": value })))
        }
        Err(e) => {
            let mut ds = DEMO_STATE.write().await;
            ds.scenario_status.insert(name.clone(), "error".to_string());
            Ok(Json(json!({ "status": "error", "error": e })))
        }
    }
}

/// GET /api/v1/demo/scenario/:name
pub async fn get_scenario(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if state.demo_config.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let ds = DEMO_STATE.read().await;
    let status = ds.scenario_status.get(&name).cloned().unwrap_or_else(|| "ready".to_string());
    let result = ds.scenario_results.get(&name).cloned();
    Ok(Json(json!({ "name": name, "status": status, "result": result })))
}
```

- [ ] **Step 3: Implement wizard step functions**

Add to `src/web/handlers/demo.rs`. These functions call existing internal logic:

```rust
use crate::web::state::DemoConfig;
use crate::web::db;
use crate::profile::io::read_profile;
use crate::classify::classify_session;
use crate::replay::session::run_replay;
use crate::replay::ReplayMode;

async fn run_wizard_explore(_state: &AppState, dc: &DemoConfig) -> Result<serde_json::Value, String> {
    let profile = read_profile(&dc.workload_path).map_err(|e| e.to_string())?;

    let total_queries: usize = profile.sessions.iter().map(|s| s.queries.len()).sum();
    let classifications: Vec<_> = profile.sessions.iter()
        .map(|s| classify_session(s))
        .collect();

    // Count by WorkloadClass
    let class_counts = classifications.iter().fold(
        std::collections::HashMap::new(),
        |mut acc, c| { *acc.entry(format!("{}", c.class)).or_insert(0) += 1; acc }
    );

    Ok(json!({
        "total_sessions": profile.sessions.len(),
        "total_queries": total_queries,
        "source_host": profile.source_host,
        "capture_method": profile.capture_method,
        "classification": class_counts,
    }))
}

async fn run_wizard_replay(state: &AppState, dc: &DemoConfig) -> Result<serde_json::Value, String> {
    let profile = read_profile(&dc.workload_path).map_err(|e| e.to_string())?;

    // run_replay returns Vec<ReplayResults>
    let results = run_replay(
        &profile,
        &dc.db_b,
        ReplayMode::ReadWrite,
        1.0,  // speed
    ).await.map_err(|e| e.to_string())?;

    // Serialize replay results as JSON for later steps (not as a .wkl profile)
    let results_json = serde_json::to_string(&results).map_err(|e| e.to_string())?;
    let results_path = state.data_dir.join("demo-replay-results.json");
    std::fs::write(&results_path, &results_json).map_err(|e| e.to_string())?;

    let total_queries: usize = results.iter().map(|r| r.query_results.len()).sum();
    Ok(json!({
        "status": "completed",
        "total_sessions": results.len(),
        "total_queries": total_queries,
    }))
}

async fn run_wizard_compare(state: &AppState, dc: &DemoConfig) -> Result<serde_json::Value, String> {
    let source = read_profile(&dc.workload_path).map_err(|e| e.to_string())?;

    // Load saved replay results
    let results_path = state.data_dir.join("demo-replay-results.json");
    let results_json = std::fs::read_to_string(&results_path)
        .map_err(|e| format!("Run replay first (step 2): {}", e))?;
    let results: Vec<crate::replay::ReplayResults> =
        serde_json::from_str(&results_json).map_err(|e| e.to_string())?;

    // compute_comparison(source, results, threshold_pct) -> ComparisonReport
    let report = crate::compare::compute_comparison(&source, &results, 20.0);
    Ok(serde_json::to_value(&report).map_err(|e| e.to_string())?)
}

async fn run_wizard_scale(state: &AppState, dc: &DemoConfig) -> Result<serde_json::Value, String> {
    let profile = read_profile(&dc.workload_path).map_err(|e| e.to_string())?;

    // Scale: analytical 2x, transactional 4x, mixed 1x, bulk 1x
    let mut scale_map = std::collections::HashMap::new();
    scale_map.insert(crate::classify::WorkloadClass::Analytical, 2u32);
    scale_map.insert(crate::classify::WorkloadClass::Transactional, 4u32);
    scale_map.insert(crate::classify::WorkloadClass::Mixed, 1u32);
    scale_map.insert(crate::classify::WorkloadClass::Bulk, 1u32);

    // scale_sessions_by_class returns Vec<Session>, wrap into a new WorkloadProfile
    let scaled_sessions = crate::replay::scaling::scale_sessions_by_class(&profile, &scale_map, 500);
    let mut scaled_profile = profile.clone();
    scaled_profile.sessions = scaled_sessions;

    let replay_start = std::time::Instant::now();
    let results = run_replay(
        &scaled_profile,
        &dc.db_b,
        ReplayMode::ReadWrite,
        1.0,
    ).await.map_err(|e| e.to_string())?;
    let elapsed_us = replay_start.elapsed().as_micros() as u64;

    // compute_scale_report(results, scale_factor, elapsed_us) -> ScaleReport
    let report = crate::compare::capacity::compute_scale_report(&results, 3, elapsed_us);
    Ok(serde_json::to_value(&report).map_err(|e| e.to_string())?)
}

async fn run_wizard_tune(_state: &AppState, dc: &DemoConfig) -> Result<serde_json::Value, String> {
    // Dry-run tuning — connect to DB-B, collect PG context, report what's available
    let (client, connection) = tokio_postgres::connect(&dc.db_b, tokio_postgres::NoTls)
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(async move { connection.await.ok(); });

    let profile = read_profile(&dc.workload_path).map_err(|e| e.to_string())?;

    // collect_context(client, profile, max_slow_queries) -> PgContext
    let context = crate::tuner::context::collect_context(&client, &profile, 10)
        .await
        .map_err(|e| e.to_string())?;

    Ok(json!({
        "status": "dry_run",
        "context_collected": true,
        "pg_settings_count": context.non_default_settings.len(),
        "tables_found": context.schema.len(),
        "stat_statements": context.stat_statements.is_some(),
        "hint": "To run full AI tuning, configure an LLM provider API key in the Tuning page.",
    }))
}
```

- [ ] **Step 4: Implement scenario functions**

Add to `src/web/handlers/demo.rs`:

```rust
async fn run_scenario_migration(_state: &AppState, dc: &DemoConfig) -> Result<serde_json::Value, String> {
    // Replay + compare (same as wizard steps 2+3 combined)
    let profile = read_profile(&dc.workload_path).map_err(|e| e.to_string())?;
    let results = run_replay(&profile, &dc.db_b, ReplayMode::ReadWrite, 1.0)
        .await.map_err(|e| e.to_string())?;
    let report = crate::compare::compute_comparison(&profile, &results, 20.0);
    Ok(serde_json::to_value(&report).map_err(|e| e.to_string())?)
}

async fn run_scenario_capacity(state: &AppState, dc: &DemoConfig) -> Result<serde_json::Value, String> {
    // Same as wizard step 4
    run_wizard_scale(state, dc).await
}

async fn run_scenario_ab(_state: &AppState, dc: &DemoConfig) -> Result<serde_json::Value, String> {
    use crate::compare::ab::{VariantResult, compute_ab_comparison};

    let profile = read_profile(&dc.workload_path).map_err(|e| e.to_string())?;

    // Replay against both databases (read-only for fair comparison)
    let results_a = run_replay(&profile, &dc.db_a, ReplayMode::ReadOnly, 1.0)
        .await.map_err(|e| e.to_string())?;
    let results_b = run_replay(&profile, &dc.db_b, ReplayMode::ReadOnly, 1.0)
        .await.map_err(|e| e.to_string())?;

    // Construct VariantResult using from_results()
    let variants = vec![
        VariantResult::from_results("db-a".to_string(), results_a),
        VariantResult::from_results("db-b".to_string(), results_b),
    ];

    let report = compute_ab_comparison(variants, 20.0);
    Ok(serde_json::to_value(&report).map_err(|e| e.to_string())?)
}

async fn run_scenario_tuning(state: &AppState, dc: &DemoConfig) -> Result<serde_json::Value, String> {
    // Same as wizard step 5
    run_wizard_tune(state, dc).await
}
```

- [ ] **Step 5: Register demo module in handlers/mod.rs**

Add to `src/web/handlers/mod.rs`:

```rust
pub mod demo;
```

- [ ] **Step 6: Add demo routes to routes.rs**

In `src/web/routes.rs`, add demo routes before the final `Router::new().nest(...)`:

```rust
        // Demo
        .route("/demo/config", get(handlers::demo::get_config))
        .route("/demo/reset-db", post(handlers::demo::reset_db))
        .route("/demo/wizard/{step}", post(handlers::demo::run_wizard_step))
        .route("/demo/wizard/{step}", get(handlers::demo::get_wizard_step))
        .route("/demo/scenario/{name}", post(handlers::demo::run_scenario))
        .route("/demo/scenario/{name}", get(handlers::demo::get_scenario));
```

- [ ] **Step 7: Build and verify compilation**

Run: `cargo build`
Expected: No compilation errors.

- [ ] **Step 8: Commit**

```bash
git add src/web/handlers/demo.rs src/web/handlers/mod.rs src/web/routes.rs
git commit -m "feat(web): add demo API handlers for wizard steps and scenario cards"
```

---

### Task 7: Frontend — Demo Page

**Files:**
- Create: `src/web/static/js/pages/demo.js`
- Modify: `src/web/static/js/app.js`
- Modify: `src/web/static/index.html`

- [ ] **Step 1: Create src/web/static/js/pages/demo.js**

Follow the Alpine.js component pattern used by other pages. This file implements:
- Wizard stepper (5 steps with state tracking)
- Scenario cards (4 cards with run/reset)
- Results rendering for each step/scenario

```javascript
function demoPage() {
    return {
        demoEnabled: false,
        dbA: '',
        dbB: '',
        wizardSteps: [
            { id: 1, title: 'Explore', desc: 'Inspect the demo workload and classify sessions', icon: '🔍', status: 'ready', result: null },
            { id: 2, title: 'Replay', desc: 'Replay workload against Database B', icon: '▶️', status: 'locked', result: null },
            { id: 3, title: 'Compare', desc: 'Compare source vs. replay performance', icon: '📊', status: 'locked', result: null },
            { id: 4, title: 'Scale', desc: 'Replay at 3x scale for capacity testing', icon: '📈', status: 'locked', result: null },
            { id: 5, title: 'AI Tune', desc: 'Run AI tuning advisor (dry-run)', icon: '🤖', status: 'locked', result: null },
        ],
        scenarios: [
            { name: 'migration', title: 'Migration Test', desc: 'Replay workload against DB-B and compare performance', status: 'ready', result: null },
            { name: 'capacity', title: 'Capacity Planning', desc: 'Replay at 3x scale with per-category breakdown', status: 'ready', result: null },
            { name: 'ab', title: 'A/B Comparison', desc: 'Compare DB-A vs DB-B with identical traffic', status: 'ready', result: null },
            { name: 'tuning', title: 'AI Tuning', desc: 'Run tuning advisor against DB-B (dry-run)', status: 'ready', result: null },
        ],
        resettingDb: false,

        async load() {
            const el = document.getElementById('demo-content');
            if (!el) return;

            // Check demo config
            const config = await api.get('/demo/config');
            this.demoEnabled = config.enabled;
            if (!this.demoEnabled) {
                el.innerHTML = `<div class="card p-8 text-center">
                    <h2 class="text-xl font-semibold text-slate-300 mb-2">Demo Mode Not Enabled</h2>
                    <p class="text-slate-500">Set <code class="text-accent font-mono">PG_RETEST_DEMO=true</code> to enable the demo environment.</p>
                </div>`;
                return;
            }
            this.dbA = config.db_a_host || 'db-a';
            this.dbB = config.db_b_host || 'db-b';

            // Unlock step 1
            this.wizardSteps[0].status = 'ready';

            this.render(el);
        },

        render(el) {
            el.innerHTML = `
                <div class="space-y-8">
                    <!-- Header -->
                    <div class="flex items-center justify-between">
                        <div>
                            <h1 class="text-2xl font-bold text-slate-100">Demo Environment</h1>
                            <p class="text-sm text-slate-500 mt-1">E-commerce workload • Database A (${this.escapeHtml(this.dbA)}) → Database B (${this.escapeHtml(this.dbB)})</p>
                        </div>
                        <button id="demo-reset-btn"
                            class="px-4 py-2 rounded-lg text-sm font-medium bg-slate-800 border border-slate-700 text-slate-300 hover:bg-slate-700 transition-colors"
                            onclick="document.querySelector('[x-data]').__x.$data.resetDb()">
                            Reset Database B
                        </button>
                    </div>

                    <!-- Wizard Section -->
                    <div class="card p-6">
                        <h2 class="text-lg font-semibold text-slate-200 mb-4">Guided Walkthrough</h2>
                        <p class="text-sm text-slate-500 mb-6">Step through the core workflow: capture → replay → compare → scale → tune</p>
                        <div id="demo-wizard" class="space-y-3"></div>
                    </div>

                    <!-- Scenario Cards -->
                    <div>
                        <h2 class="text-lg font-semibold text-slate-200 mb-4">Scenarios</h2>
                        <p class="text-sm text-slate-500 mb-4">Run pre-built test scenarios independently</p>
                        <div id="demo-scenarios" class="grid grid-cols-1 md:grid-cols-2 gap-4"></div>
                    </div>
                </div>
            `;
            this.renderWizard();
            this.renderScenarios();
        },

        renderWizard() {
            const el = document.getElementById('demo-wizard');
            if (!el) return;

            let html = '';
            this.wizardSteps.forEach((step, idx) => {
                const isActive = step.status === 'ready' || step.status === 'completed';
                const isRunning = step.status === 'running';
                const isCompleted = step.status === 'completed';
                const isLocked = step.status === 'locked';

                html += `
                <div class="flex items-start gap-4 p-4 rounded-lg border ${isCompleted ? 'border-accent/30 bg-accent/5' : isRunning ? 'border-amber-400/30 bg-amber-400/5' : isLocked ? 'border-slate-800 bg-slate-900/50 opacity-50' : 'border-slate-700 bg-slate-800/50'}">
                    <div class="flex-shrink-0 w-10 h-10 rounded-full flex items-center justify-center text-lg
                        ${isCompleted ? 'bg-accent/20 text-accent' : isRunning ? 'bg-amber-400/20 text-amber-400' : 'bg-slate-700 text-slate-400'}">
                        ${isCompleted ? '✓' : step.icon}
                    </div>
                    <div class="flex-1 min-w-0">
                        <div class="flex items-center gap-2 mb-1">
                            <span class="font-semibold text-sm ${isLocked ? 'text-slate-600' : 'text-slate-200'}">Step ${step.id}: ${step.title}</span>
                            ${isRunning ? '<span class="badge badge-warning"><span class="spinner" style="width:0.7em;height:0.7em"></span> Running</span>' : ''}
                            ${isCompleted ? '<span class="badge badge-success">Complete</span>' : ''}
                        </div>
                        <p class="text-xs text-slate-500 mb-2">${step.desc}</p>
                        ${isActive && !isRunning ? `<button class="px-3 py-1.5 rounded text-xs font-medium bg-accent/20 text-accent border border-accent/30 hover:bg-accent/30 transition-colors" data-step="${step.id}">
                            ${isCompleted ? 'Re-run' : 'Run Step'}
                        </button>` : ''}
                        ${step.result ? `<div class="mt-3 p-3 rounded bg-slate-900/50 border border-slate-800 text-xs font-mono text-slate-400 overflow-x-auto" id="wizard-result-${step.id}"></div>` : ''}
                    </div>
                </div>`;
            });
            el.innerHTML = html;

            // Attach click handlers
            el.querySelectorAll('[data-step]').forEach(btn => {
                btn.addEventListener('click', () => this.runWizardStep(parseInt(btn.dataset.step)));
            });

            // Render results
            this.wizardSteps.forEach(step => {
                if (step.result) {
                    const resEl = document.getElementById(`wizard-result-${step.id}`);
                    if (resEl) resEl.textContent = JSON.stringify(step.result, null, 2);
                }
            });
        },

        renderScenarios() {
            const el = document.getElementById('demo-scenarios');
            if (!el) return;

            let html = '';
            this.scenarios.forEach(scenario => {
                const isRunning = scenario.status === 'running';
                const isCompleted = scenario.status === 'completed';

                html += `
                <div class="card p-5">
                    <div class="flex items-center justify-between mb-2">
                        <h3 class="font-semibold text-sm text-slate-200">${scenario.title}</h3>
                        ${isRunning ? '<span class="badge badge-warning"><span class="spinner" style="width:0.7em;height:0.7em"></span> Running</span>'
                            : isCompleted ? '<span class="badge badge-success">Complete</span>'
                            : '<span class="badge badge-info">Ready</span>'}
                    </div>
                    <p class="text-xs text-slate-500 mb-4">${scenario.desc}</p>
                    <div class="flex gap-2">
                        ${!isRunning ? `<button class="px-3 py-1.5 rounded text-xs font-medium bg-accent/20 text-accent border border-accent/30 hover:bg-accent/30 transition-colors" data-scenario="${scenario.name}">Run</button>` : ''}
                        ${isCompleted ? `<button class="px-3 py-1.5 rounded text-xs font-medium bg-slate-800 text-slate-400 border border-slate-700 hover:bg-slate-700 transition-colors" data-scenario-reset="${scenario.name}">Reset</button>` : ''}
                    </div>
                    ${scenario.result ? `<div class="mt-3 p-3 rounded bg-slate-900/50 border border-slate-800 text-xs font-mono text-slate-400 overflow-x-auto max-h-60 overflow-y-auto" id="scenario-result-${scenario.name}"></div>` : ''}
                </div>`;
            });
            el.innerHTML = html;

            // Attach click handlers
            el.querySelectorAll('[data-scenario]').forEach(btn => {
                btn.addEventListener('click', () => this.runScenario(btn.dataset.scenario));
            });
            el.querySelectorAll('[data-scenario-reset]').forEach(btn => {
                btn.addEventListener('click', () => this.resetScenario(btn.dataset.scenarioReset));
            });

            // Render results
            this.scenarios.forEach(scenario => {
                if (scenario.result) {
                    const resEl = document.getElementById(`scenario-result-${scenario.name}`);
                    if (resEl) resEl.textContent = JSON.stringify(scenario.result, null, 2);
                }
            });
        },

        async runWizardStep(step) {
            const idx = step - 1;
            this.wizardSteps[idx].status = 'running';
            this.wizardSteps[idx].result = null;
            this.renderWizard();

            try {
                const res = await api.post(`/demo/wizard/${step}`, {});
                this.wizardSteps[idx].status = 'completed';
                this.wizardSteps[idx].result = res.result || res;

                // Unlock next step
                if (idx + 1 < this.wizardSteps.length && this.wizardSteps[idx + 1].status === 'locked') {
                    this.wizardSteps[idx + 1].status = 'ready';
                }
                window.showToast(`Step ${step} completed`, 'success');
            } catch (e) {
                this.wizardSteps[idx].status = 'ready';
                this.wizardSteps[idx].result = { error: e.message || 'Step failed' };
                window.showToast(`Step ${step} failed: ${e.message}`, 'error');
            }
            this.renderWizard();
        },

        async runScenario(name) {
            const scenario = this.scenarios.find(s => s.name === name);
            if (!scenario) return;
            scenario.status = 'running';
            scenario.result = null;
            this.renderScenarios();

            try {
                const res = await api.post(`/demo/scenario/${name}`, {});
                scenario.status = 'completed';
                scenario.result = res.result || res;
                window.showToast(`${scenario.title} completed`, 'success');
            } catch (e) {
                scenario.status = 'ready';
                scenario.result = { error: e.message || 'Scenario failed' };
                window.showToast(`${scenario.title} failed: ${e.message}`, 'error');
            }
            this.renderScenarios();
        },

        resetScenario(name) {
            const scenario = this.scenarios.find(s => s.name === name);
            if (!scenario) return;
            scenario.status = 'ready';
            scenario.result = null;
            this.renderScenarios();
        },

        async resetDb() {
            if (this.resettingDb) return;
            this.resettingDb = true;
            const btn = document.getElementById('demo-reset-btn');
            if (btn) { btn.textContent = 'Resetting...'; btn.disabled = true; }

            try {
                await api.post('/demo/reset-db', {});
                window.showToast('Database B reset to initial state', 'success');
            } catch (e) {
                window.showToast(`Reset failed: ${e.message}`, 'error');
            }

            this.resettingDb = false;
            if (btn) { btn.textContent = 'Reset Database B'; btn.disabled = false; }
        },

        escapeHtml(str) {
            if (!str) return '';
            const map = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#039;' };
            return str.replace(/[&<>"']/g, m => map[m]);
        },
    };
}
```

- [ ] **Step 2: Add demo nav item to app.js**

In `src/web/static/js/app.js`, add the demo nav item to the `navItems` array. It should be conditionally visible. Add it after the `dashboard` entry:

```javascript
{ id: 'demo', label: 'Demo', icon: '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="5 3 19 12 5 21 5 3"></polygon></svg>' },
```

Also add a `demoEnabled` state property and fetch demo config in the `init()` function:

```javascript
// In init():
api.get('/demo/config').then(res => {
    this.demoEnabled = res.enabled || false;
}).catch(() => {
    this.demoEnabled = false;
});
```

Update the nav template to conditionally show the demo item:

```javascript
// In the nav template, add x-show condition for demo:
// :class should include: item.id === 'demo' && !demoEnabled ? 'hidden' : ''
```

- [ ] **Step 3: Add demo page section to index.html**

Add the demo page block alongside the other page sections:

```html
<!-- Demo -->
<div x-show="page === 'demo'" x-data="demoPage()" x-effect="if (page === 'demo') load()">
    <div id="demo-content"></div>
</div>
```

Add the script tag for the demo page JS:

```html
<script src="/js/pages/demo.js"></script>
```

- [ ] **Step 4: Build and verify**

Run: `cargo build`
Expected: No errors (static files are embedded at compile time via rust-embed).

- [ ] **Step 5: Commit**

```bash
git add src/web/static/js/pages/demo.js src/web/static/js/app.js src/web/static/index.html
git commit -m "feat(web): add demo page with guided wizard and scenario cards"
```

---

### Task 8: Documentation — AI Tuning Guide

**Files:**
- Create: `docs/tuning.md`

- [ ] **Step 1: Write docs/tuning.md**

Follow the pattern used by `docs/replay.md` and `docs/web-dashboard.md`: overview, quick start, key concepts, feature sections with examples, CLI reference table.

Content should cover:
- Quick start (dry-run and apply modes)
- How the tuning loop works (context collection → LLM → safety → apply → replay → compare → rollback)
- Recommendation types (config, index, query rewrite, schema)
- Safety features (allowlist, production hostname check, auto-rollback)
- LLM providers (Claude, OpenAI, Gemini, Bedrock, Ollama) with API key setup
- CLI reference (all flags with defaults)
- Web dashboard tuning page
- Examples (dry-run, apply with rollback, multi-iteration, custom hint)

Estimated length: ~250-300 lines.

- [ ] **Step 2: Commit**

```bash
git add docs/tuning.md
git commit -m "docs: add AI-assisted tuning guide"
```

---

### Task 9: Documentation — Workload Transform Guide

**Files:**
- Create: `docs/transform.md`

- [ ] **Step 1: Write docs/transform.md**

Cover the 3-layer architecture:
- Overview of analyze → plan → apply workflow
- Analyzer (deterministic workload analysis, table grouping, Union-Find)
- Planner (multi-provider LLM, prompt engineering, TOML plan output)
- Engine (deterministic application: scale, inject, inject_session, remove)
- Transform plan format (TOML with examples)
- LLM providers (same as tuning)
- CLI reference (all subcommands and flags)
- Web dashboard transform page
- Examples (analyze, generate plan, apply plan, dry-run)

Estimated length: ~250-300 lines.

- [ ] **Step 2: Commit**

```bash
git add docs/transform.md
git commit -m "docs: add workload transform guide"
```

---

### Task 10: Integration Testing

**Files:**
- Create: `tests/demo_config_test.rs`

- [ ] **Step 1: Write integration tests for DemoConfig**

```rust
use pg_retest::web::state::DemoConfig;
use std::path::PathBuf;

#[test]
fn test_demo_config_disabled_by_default() {
    std::env::remove_var("PG_RETEST_DEMO");
    let config = DemoConfig::from_env();
    assert!(config.is_none());
}

#[test]
fn test_demo_config_requires_db_strings() {
    std::env::set_var("PG_RETEST_DEMO", "true");
    std::env::remove_var("DEMO_DB_A");
    std::env::remove_var("DEMO_DB_B");
    let config = DemoConfig::from_env();
    assert!(config.is_none());
    std::env::remove_var("PG_RETEST_DEMO");
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --test demo_config_test`
Expected: PASS

- [ ] **Step 3: Run full test suite**

Run: `cargo test`
Expected: All tests pass (existing + new).

- [ ] **Step 4: Run clippy and fmt**

Run: `cargo clippy && cargo fmt --check`
Expected: Zero warnings, no formatting issues.

- [ ] **Step 5: Commit**

```bash
git add tests/demo_config_test.rs
git commit -m "test: add integration tests for demo mode configuration"
```

---

### Task 11: Generate Demo Workload

**Files:**
- Create: `demo/workload.wkl` (binary, generated)

- [ ] **Step 1: Run the workload generator**

Run: `cd /Users/matt.yonkovit/yonk-tools/pg-retest && bash demo/generate-workload.sh`
Expected: Script starts PG container, builds pg-retest, runs proxy, executes sessions, produces `demo/workload.wkl`.

- [ ] **Step 2: Verify the workload**

Run: `cargo run -- inspect demo/workload.wkl --classify`
Expected: Shows 8-10 sessions, mix of Analytical/Transactional/Mixed/Bulk classifications, ~100-500 queries.

- [ ] **Step 3: Commit the workload file**

```bash
git add demo/workload.wkl
git commit -m "feat(demo): add pre-built e-commerce demo workload"
```

---

### Task 12: End-to-End Docker Validation

- [ ] **Step 1: Build and start the full Docker environment**

Run: `docker compose up --build -d`
Expected: All three services start, health checks pass.

- [ ] **Step 2: Verify web dashboard loads**

Run: `curl -s http://localhost:8080/api/v1/health | jq .`
Expected: `{ "status": "ok", "version": "...", "name": "pg-retest" }`

- [ ] **Step 3: Verify demo config is enabled**

Run: `curl -s http://localhost:8080/api/v1/demo/config | jq .`
Expected: `{ "enabled": true, "db_a_host": "db-a", "db_b_host": "db-b" }`

- [ ] **Step 4: Test demo wizard step 1**

Run: `curl -s -X POST http://localhost:8080/api/v1/demo/wizard/1 | jq .`
Expected: Returns workload inspection with session count, query count, classifications.

- [ ] **Step 5: Test DB reset**

Run: `curl -s -X POST http://localhost:8080/api/v1/demo/reset-db | jq .`
Expected: `{ "status": "reset_complete" }`

- [ ] **Step 6: Clean up**

Run: `docker compose down -v`

- [ ] **Step 7: Add Docker quick start to README**

Add a "Try with Docker" section to README after Quick Start:

```markdown
### Try with Docker

```bash
# Start the full demo environment (two PostgreSQL databases + web dashboard)
docker compose up --build

# Open http://localhost:8080 — click "Demo" in the sidebar
# Tear down when done:
docker compose down -v
```
```

- [ ] **Step 8: Commit**

```bash
git add README.md
git commit -m "docs: add Docker quick start section to README"
```

---

### Task 13: Update CLAUDE.md

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add demo module documentation to CLAUDE.md**

Add to the Key modules section:
```
- `web::handlers::demo` — Demo mode handlers (wizard steps, scenario cards, DB reset)
```

Add to Gotchas:
```
- Demo mode: requires `PG_RETEST_DEMO=true` env var. Connection strings via `DEMO_DB_A`, `DEMO_DB_B`. Workload path via `DEMO_WORKLOAD`.
- Demo page: wizard step state and scenario results are stored in-memory (reset on server restart).
- Demo DB reset: drops and recreates all tables in DB-B by re-running init-db-b.sql.
```

Add to Architecture section (under Web Dashboard):
```
- **Docker Demo** — docker-compose with pg-retest + db-a (seeded) + db-b (seeded). Demo page with 5-step wizard + 4 scenario cards.
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: update CLAUDE.md with demo mode documentation"
```
