# C Driver Test

Uses libpq directly to test pg-retest proxy capture with parameterized queries.

## Prerequisites

Install libpq development headers:

```bash
# Debian/Ubuntu
sudo apt-get install libpq-dev

# Fedora/RHEL
sudo dnf install libpq-devel

# macOS
brew install libpq
```

## Compile and Run

```bash
make

# Default: connects to localhost:5433 (proxy)
./test_driver

# Custom connection:
DATABASE_URL="host=localhost port=5433 dbname=ecommerce user=demo password=demo" ./test_driver
```
