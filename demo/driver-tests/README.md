# Multi-Language Driver Tests

Test applications in 5 languages that exercise pg-retest proxy capture and replay through each language's PostgreSQL driver.

## What These Tests Do

Each driver test connects through the pg-retest proxy and runs the same sequence of operations:

1. **INSERT** a customer (bare INSERT, no RETURNING)
2. **SELECT currval** to get the generated customer ID
3. **INSERT** an order using that customer ID (cross-table FK reference)
4. **INSERT** an order item linked to the order (FK chain)
5. **SELECT** the order back and verify the customer ID matches
6. **UPDATE** the order status
7. **INSERT** a tracking event (UUID primary key table)
8. **COMMIT** the transaction

This exercises the key patterns that matter for capture/replay fidelity:
- SERIAL/sequence-generated IDs via `currval()`
- Cross-table foreign key references using captured IDs
- Parameterized queries (extended query protocol) in each driver
- UUID primary keys (server-generated)
- Mix of INSERT, SELECT, UPDATE within a transaction

## Languages

| Language | Driver | Directory |
|----------|--------|-----------|
| Python | psycopg2 | `python/` |
| Go | pgx/v5 | `go/` |
| Node.js | pg (node-postgres) | `node/` |
| Java | PostgreSQL JDBC | `java/` |
| C | libpq | `c/` |

## Running Individually

Each subdirectory has its own README with setup and run instructions. All tests default to `localhost:5433` (the proxy port) and can be configured via the `DATABASE_URL` environment variable.

## Running All via Docker Compose

Assumes the pg-retest proxy is already running on the host:

```bash
# Start the proxy first (e.g., from the project root)
cargo run -- proxy --listen 0.0.0.0:5433 --target localhost:5432 --dbname ecommerce

# Then run all driver tests
cd demo/driver-tests
docker compose up --build
```

## Expected Output

Each test prints its progress and ends with:

```
  All operations completed successfully!
```

Or on failure:

```
  FAILED: <error details>
```
