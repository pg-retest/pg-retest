# Synthetic Data Generation

## Why Synthetic Data?

When you use `pg-retest transform` to reshape a captured workload — scaling up product catalog reads, injecting checkout stress, or simulating a Black Friday spike — the resulting `.wkl` file contains SQL queries that expect matching data in the target database. If the target is empty or has a different schema, the queries return zero rows, hit FK violations, or produce misleading performance numbers.

The synthetic data generator bridges this gap. It analyzes both the workload queries and the source database schema to produce a SQL dump that creates tables with the right structure and populates them with enough realistic-looking data for the workload to execute successfully.

## Quick Start

```bash
# 1. Generate synthetic data matching a workload
python3 demo/synthetic-data-gen.py \
    --workload workload.wkl \
    --source-db "host=localhost port=5450 dbname=ecommerce user=demo password=demo" \
    --output synthetic-data.sql

# 2. Load into the target database
psql "host=target-host dbname=testdb user=admin" < synthetic-data.sql

# 3. Replay the workload against the populated target
pg-retest replay workload.wkl \
    --target "host=target-host dbname=testdb user=admin"
```

## Recommended Workflow

The synthetic data generator fits into the transform pipeline between plan application and replay:

```
capture workload.wkl (from production or proxy)
        |
pg-retest transform analyze --workload workload.wkl
        |
pg-retest transform plan --workload workload.wkl --prompt "..." --output plan.toml
        |
pg-retest transform apply --workload workload.wkl --plan plan.toml --output transformed.wkl
        |
python3 demo/synthetic-data-gen.py \                   <-- this step
    --workload transformed.wkl \
    --source-db "..." \
    --output synthetic-data.sql
        |
psql "target-db" < synthetic-data.sql
        |
pg-retest replay transformed.wkl --target "target-db"
        |
pg-retest compare --source workload.wkl --replay replay-results.wkl
```

### When to Use It

- **After transform** — The transformed workload may reference tables or rows that don't exist on a fresh target.
- **Cross-database migration** — After converting MySQL queries to PostgreSQL via `pg-retest transform`, you need a PostgreSQL database with matching data.
- **Load testing on a clean instance** — Spin up a new database, load synthetic data, and replay at scale without needing production backups.
- **CI/CD pipelines** — Generate deterministic test data (via `--seed`) as part of an automated pipeline.

### When NOT to Use It

- **Exact replay against a backup** — If you restored from a point-in-time backup, the real data is already there. Synthetic data is unnecessary.
- **Read-only replay** — If the workload is read-only and the target already has data, you don't need synthetic generation.

## How It Works

The generator has three stages:

### 1. Workload Analysis

Parses the `.wkl` file (via `pg-retest inspect --output-format json`) and extracts:

- **Table names** — from `FROM`, `JOIN`, `INSERT INTO`, `UPDATE` clauses
- **Value ranges** — numeric literals in `WHERE` conditions
- **Row count hints** — from `LIMIT` clauses

### 2. Schema Inspection

Connects to the source database and collects:

- **DDL** — column types, defaults, nullable constraints
- **Foreign keys** — parent-child relationships between tables
- **Unique constraints** — columns that need distinct values
- **Column statistics** — row counts, min/max values, value distributions for low-cardinality columns (e.g., status fields)
- **Indexes** — recreated on the target for realistic query plans

### 3. Data Generation

Produces a SQL file that:

- Creates tables in FK-dependency order (parents before children)
- Generates rows matching the source row count (scaled by `--scale`)
- Preserves value distributions — if 55% of orders are "shipped" in the source, the synthetic data does too
- Ensures FK consistency — child rows reference existing parent IDs
- Uses realistic fake values (names, emails, timestamps) based on column name heuristics
- Resets sequences so subsequent INSERTs get correct IDs
- Recreates indexes and runs `ANALYZE` for fresh planner statistics

## CLI Reference

```
python3 demo/synthetic-data-gen.py [OPTIONS]

Required:
  --workload PATH       Path to .wkl workload file
  --source-db DSN       Connection string for the source database
  --output PATH         Output path for the generated SQL file

Optional:
  --scale FLOAT         Scale factor for row counts (default: 1.0)
  --seed INT            Random seed for reproducibility (default: 42)
  --tables TABLE [...]  Specific tables to include (default: auto-detect)
  --verbose, -v         Print detailed progress
```

### Scale Factor

The `--scale` parameter multiplies the source row counts:

| Scale | Source 5,000 rows | Source 50,000 rows |
|-------|------------------|--------------------|
| 0.1   | 500              | 5,000              |
| 1.0   | 5,000            | 50,000             |
| 3.0   | 15,000           | 150,000            |
| 10.0  | 50,000           | 500,000            |

Use small scales (0.1-0.5) for quick iteration. Use larger scales (5-10x) when testing how the workload performs against a bigger dataset.

### Reproducibility

The `--seed` parameter ensures identical output for the same inputs. Two runs with the same seed, scale, and source schema produce byte-identical SQL files. This is useful for CI pipelines where you want deterministic test data.

## Limitations

**Statistical approximation, not exact replica.** The generator preserves distributions and value ranges but does not reproduce the exact rows from the source. Queries with hardcoded IDs (e.g., `WHERE id = 42`) may not match if row 42 doesn't exist in the synthetic data.

**Regex-based table extraction.** Table names are extracted from SQL with simple regex patterns. Complex subqueries, CTEs, or dynamically constructed SQL may not be fully parsed.

**No CHECK constraint interpretation.** While CHECK constraints are detected, the generator does not parse arbitrary CHECK expressions to constrain values. It relies on column name heuristics and source statistics instead. A `CHECK (rating BETWEEN 1 AND 5)` is handled by the min/max stats from the source, not by parsing the CHECK clause.

**SERIAL columns only.** The generator assumes `id` columns are auto-incrementing serials. IDENTITY columns, UUID primary keys, and composite keys are handled but may need manual adjustment in edge cases.

**Single-schema support.** Only the `public` schema is inspected. Multi-schema databases require manual `--tables` specification or script modification.

**No GIS/array/composite types.** Exotic PostgreSQL types (geometry, arrays, hstore, custom composites) fall back to a generic string value. Extend the `_generate_value` method for domain-specific types.

## Dependencies

The script requires Python 3.8+ and the `psycopg2` driver:

```bash
pip install psycopg2-binary
```

The `pg-retest` binary must be on `PATH` for workload inspection. Build it with:

```bash
cargo build --release
export PATH="$PWD/target/release:$PATH"
```
