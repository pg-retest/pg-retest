# Node.js Driver Test

Uses `pg` (node-postgres) to test pg-retest proxy capture with parameterized queries.

## Prerequisites

```bash
npm install
```

## Run

```bash
# Default: connects to localhost:5433 (proxy)
node test_driver.js

# Custom connection:
DATABASE_URL="host=localhost port=5433 dbname=ecommerce user=demo password=demo" node test_driver.js
```
