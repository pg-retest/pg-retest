# Go Driver Test

Uses `pgx/v5` (the standard Go PostgreSQL driver) to test pg-retest proxy capture.

## Prerequisites

```bash
go mod tidy
```

## Run

```bash
# Default: connects to localhost:5433 (proxy)
go run main.go

# Custom connection:
DATABASE_URL="host=localhost port=5433 dbname=ecommerce user=demo password=demo" go run main.go
```
