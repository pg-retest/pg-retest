# Demo Environment Guide

pg-retest ships with a self-contained Docker demo that lets you explore every feature without setting up your own databases or capturing real traffic. The demo includes two PostgreSQL 16 databases seeded with an e-commerce dataset and a pre-built workload ready to replay, compare, scale, and tune.

## Prerequisites

- [Docker](https://docs.docker.com/get-docker/) (20.10+)
- [Docker Compose](https://docs.docker.com/compose/install/) (v2+)
- ~2GB disk space (Rust build image + PostgreSQL images)

## Quick Start

```bash
git clone https://github.com/your-org/pg-retest.git
cd pg-retest

# Build and start the demo (first build takes ~5 minutes)
docker compose up --build

# Open the dashboard
open http://localhost:8080
```

Click **Demo** in the sidebar to begin.

## What's Included

### Two PostgreSQL Databases

| Database | Hostname | Port | Purpose |
|----------|----------|------|---------|
| **Database A** | `db-a` | 5450 (host) | Source — seeded with e-commerce data |
| **Database B** | `db-b` | 5451 (host) | Target — identical data, used for replay |

Both databases contain the same e-commerce schema:

| Table | Rows | Description |
|-------|------|-------------|
| `customers` | 5,000 | Names, emails, signup dates |
| `products` | 1,000 | 10 categories, prices, stock levels |
| `orders` | 20,000 | Linked to customers, 4 statuses |
| `order_items` | 60,000 | ~3 items per order |
| `reviews` | 8,000 | Ratings 1–5 with review text |

Total: **94,000 rows** across 5 tables with foreign keys and composite indexes.

### Pre-Built Workload

A `workload.wkl` file captured from 8 concurrent sessions running a mix of:

- **Analytical queries** — Revenue reports, category breakdowns, customer lifetime value
- **Transactional queries** — Order placement with BEGIN/COMMIT, stock updates
- **Mixed queries** — Customer lookups with light writes, review submissions
- **Bulk operations** — Batch order status updates across partitions

357 queries total, classified as: 2 Analytical, 3 Transactional, 3 Mixed sessions.

## Demo Page Walkthrough

The Demo page has two sections: a **Guided Wizard** for first-time users and **Scenario Cards** for quick exploration.

### Guided Wizard

The wizard walks you through the core pg-retest workflow in 5 steps. Each step builds on the previous one.

#### Step 1: Explore

Loads the demo workload and shows:
- Total sessions and queries
- Workload classification breakdown (Analytical/Transactional/Mixed/Bulk)
- Capture metadata (source host, method, duration)

This is the same output you'd get from `pg-retest inspect workload.wkl --classify`.

#### Step 2: Replay

Replays all 357 queries against Database B, preserving the original 8 concurrent sessions and transaction boundaries. You'll see:
- Sessions replayed and query count
- Error count (should be 0 with matching schema)

#### Step 3: Compare

Compares the original capture timing against the replay results:
- Latency percentiles (p50, p95, p99)
- Per-query regression list (queries that slowed down beyond the threshold)
- Total error count

> **Note:** Some latency differences are expected — the demo databases run in Docker containers, not on the original hardware.

#### Step 4: Scale

Replays the workload with per-category scaling to simulate increased traffic:
- Analytical sessions: 2x
- Transactional sessions: 4x
- Mixed/Bulk sessions: 1x

This demonstrates capacity planning — you'll see throughput (QPS), latency under load, and whether the database handles the increased traffic.

#### Step 5: AI Tune (Dry-Run)

Connects to Database B and collects PostgreSQL context:
- Non-default `pg_settings`
- Table schemas and indexes
- `pg_stat_statements` (if available)
- EXPLAIN plans for top queries

This is the first step of the AI tuning loop. To get actual LLM recommendations, configure an API key in the Tuning page. The demo runs in dry-run mode by default.

### Scenario Cards

Four pre-built scenarios you can run independently:

| Scenario | What It Does |
|----------|-------------|
| **Migration Test** | Replays workload against DB-B and produces a comparison report. Simulates validating a database migration. |
| **Capacity Planning** | Replays at 3x scale with per-category breakdown. Find where your database breaks under load. |
| **A/B Comparison** | Replays the same workload against both DB-A and DB-B, then compares results. Simulates evaluating two database configurations. |
| **AI Tuning** | Collects database context for the AI tuning advisor (dry-run). Add an API key to get full recommendations. |

### Reset Database B

The **Reset Database B** button at the top of the Demo page drops all tables in DB-B and re-creates them with the original seed data. Use this after running write-heavy scenarios (replay, migration, capacity) to restore a clean state.

## Connecting Directly

You can also connect to the demo databases with `psql` or any PostgreSQL client:

```bash
# Connect to Database A
PGPASSWORD=demo psql -h localhost -p 5450 -U demo -d ecommerce

# Connect to Database B
PGPASSWORD=demo psql -h localhost -p 5451 -U demo -d ecommerce
```

Connection details: user `demo`, password `demo`, database `ecommerce`.

## Using the CLI with Demo Databases

The demo databases are accessible from your host machine, so you can run pg-retest CLI commands against them:

```bash
# Inspect the demo workload
pg-retest inspect demo/workload.wkl --classify

# Replay against DB-B directly
pg-retest replay \
  --workload demo/workload.wkl \
  --target "host=localhost port=5451 dbname=ecommerce user=demo password=demo" \
  --output results.wkl

# Compare source vs. replay
pg-retest compare \
  --source demo/workload.wkl \
  --replay results.wkl \
  --threshold 20

# A/B test: DB-A vs DB-B
pg-retest ab \
  --workload demo/workload.wkl \
  --variant "db-a=host=localhost port=5450 dbname=ecommerce user=demo password=demo" \
  --variant "db-b=host=localhost port=5451 dbname=ecommerce user=demo password=demo" \
  --read-only

# Capture live traffic through the proxy
pg-retest proxy \
  --listen 0.0.0.0:5433 \
  --target localhost:5450 \
  --output my-capture.wkl
# Then point your app at localhost:5433 instead of 5450

# AI tuning (dry-run, no API key needed)
pg-retest tune \
  --workload demo/workload.wkl \
  --target "host=localhost port=5451 dbname=ecommerce user=demo password=demo" \
  --provider claude
```

## Regenerating the Demo Workload

The pre-built `demo/workload.wkl` is committed to the repository. If you want to regenerate it (e.g., after modifying the session scripts):

```bash
bash demo/generate-workload.sh
```

This script:
1. Starts a temporary PostgreSQL container with the e-commerce seed data
2. Builds pg-retest from source
3. Starts the capture proxy
4. Runs all 8 session scripts in parallel through the proxy
5. Stops the proxy and saves the captured workload

The session scripts are in `demo/sessions/`:

| Script | Type | Queries |
|--------|------|---------|
| `analytical-1.sql` | SELECT-only | Revenue, category, customer analytics |
| `analytical-2.sql` | SELECT-only | Product performance, inventory, reviews |
| `transactional-1.sql` | BEGIN/COMMIT | Order placement, stock updates |
| `transactional-2.sql` | BEGIN/COMMIT | Order placement, different customers |
| `transactional-3.sql` | BEGIN/COMMIT | Orders + status processing |
| `mixed-1.sql` | Read + write | Customer lookups, review submissions |
| `mixed-2.sql` | Read + write | Customer activity, email updates |
| `bulk-1.sql` | UPDATE batches | Order status transitions in chunks |

## Teardown

```bash
# Stop and remove containers + volumes
docker compose down -v
```

This removes all containers, networks, and the named volume. Database state is not persisted between runs.

## Troubleshooting

**Build fails with Cargo.lock version error:**
The Dockerfile uses Rust 1.85. If your local Cargo.lock was generated by a newer Rust version, you may need to update the Dockerfile's `FROM rust:1.85-bookworm` to match your local toolchain.

**Port conflicts:**
The demo uses ports 5450, 5451, and 8080. If these are in use, edit `docker-compose.yml` to change the host-side port mappings.

**Demo page not showing:**
The Demo nav item only appears when `PG_RETEST_DEMO=true` is set. This is configured automatically in `docker-compose.yml`. If running pg-retest outside Docker, set the environment variables manually:

```bash
export PG_RETEST_DEMO=true
export DEMO_DB_A="host=localhost port=5450 dbname=ecommerce user=demo password=demo"
export DEMO_DB_B="host=localhost port=5451 dbname=ecommerce user=demo password=demo"
export DEMO_WORKLOAD="demo/workload.wkl"
pg-retest web --port 8080
```

**Database B has stale data after replay:**
Click **Reset Database B** on the Demo page, or restart the containers with `docker compose down -v && docker compose up -d`.
