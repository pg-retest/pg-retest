# Python Driver Test

Uses `psycopg2` to test pg-retest proxy capture with parameterized queries (extended query protocol).

## Prerequisites

```bash
pip install psycopg2-binary
```

## Run

```bash
# Default: connects to localhost:5433 (proxy)
python test_driver.py

# Custom connection:
DATABASE_URL="host=localhost port=5433 dbname=ecommerce user=demo password=demo" python test_driver.py
```
