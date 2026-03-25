# Java Driver Test

Uses JDBC with the PostgreSQL driver to test pg-retest proxy capture.

## Prerequisites

Download the PostgreSQL JDBC driver:

```bash
curl -L -o postgresql.jar https://jdbc.postgresql.org/download/postgresql-42.7.4.jar
```

## Compile and Run

```bash
# Compile
javac TestDriver.java

# Run (default: localhost:5433)
java -cp .:postgresql.jar TestDriver

# Custom connection:
PGHOST=localhost PGPORT=5433 PGDATABASE=ecommerce PGUSER=demo PGPASSWORD=demo \
  java -cp .:postgresql.jar TestDriver
```
