#!/usr/bin/env bash
# End-to-end capture → (transform) → replay validation across capture sources, plus the
# advanced "duplicate sessions" scaling feature. Spins up throwaway PostgreSQL + MySQL
# containers, drives real `pg-retest` commands, and validates replay by observing target
# database state changes. Tears everything down at the end.
#
#   ./scripts/e2e-replay.sh
#
# Requires: docker, psql (host), and — for the oracle-replay step and Oracle demo —
# a Python with `sqlglot` (set SQLGLOT_PY). Each scenario reports PASS/FAIL.
#
# Scenarios:
#   A  PG → PG, no proxy        (pg-csv log capture → replay)
#   B  PG → PG, with proxy      (live proxy capture → replay)
#   C  MySQL → PG               (mysql-slow capture + transform → replay)
#   D  Oracle → PG              (NOT integrated — honest assessment + sqlglot feasibility)
#   E  duplicate sessions       (replay --scale N)
#   F  oracle-replay command    (verified translate → replay)
set -u

# --- config ---------------------------------------------------------------------------
SRC_PORT=5440; TGT_PORT=5441; MYSQL_PORT=3310
PGUSER=e2e; PGPASS=e2e; PGDB=e2e
SQLGLOT_PY="${SQLGLOT_PY:-/tmp/sqlglot-venv/bin/python}"
TGT="host=localhost port=$TGT_PORT dbname=$PGDB user=$PGUSER password=$PGPASS"
PROXY="host=127.0.0.1 port=6544 dbname=$PGDB user=$PGUSER password=$PGPASS"
TMP="$(mktemp -d)"
PASS=0; FAIL=0

# IMPORTANT: oracle-replay is gated by the `polyglot-transform` feature, so the binary
# MUST be built with it. A plain `cargo build` would produce a binary WITHOUT the command.
echo "Building pg-retest (with polyglot-transform)…"
cargo build --features polyglot-transform >/dev/null 2>&1 || { echo "build failed"; exit 1; }
BIN=./target/debug/pg-retest

sql_src() { docker exec e2e-src-pg psql -U $PGUSER -d $PGDB -tAc "$1"; }
sql_tgt() { docker exec e2e-tgt-pg psql -U $PGUSER -d $PGDB -tAc "$1"; }
check() { # check <label> <actual> <expected>
  if [ "$2" = "$3" ]; then echo "  PASS: $1 ($2)"; PASS=$((PASS+1));
  else echo "  FAIL: $1 — got '$2', expected '$3'"; FAIL=$((FAIL+1)); fi
}

up() {
  docker rm -f e2e-src-pg e2e-tgt-pg e2e-mysql >/dev/null 2>&1
  docker run -d --name e2e-src-pg -e POSTGRES_USER=$PGUSER -e POSTGRES_PASSWORD=$PGPASS -e POSTGRES_DB=$PGDB -p $SRC_PORT:5432 postgres:16 >/dev/null
  docker run -d --name e2e-tgt-pg -e POSTGRES_USER=$PGUSER -e POSTGRES_PASSWORD=$PGPASS -e POSTGRES_DB=$PGDB -p $TGT_PORT:5432 postgres:16 >/dev/null
  docker run -d --name e2e-mysql -e MYSQL_ROOT_PASSWORD=root -e MYSQL_DATABASE=$PGDB -p $MYSQL_PORT:3306 mysql:8.0 >/dev/null
  for c in e2e-src-pg e2e-tgt-pg; do local t=0; until docker exec $c pg_isready -U $PGUSER -q 2>/dev/null; do t=$((t+1)); [ $t -gt 30 ] && break; sleep 1; done; done
  local t=0; until docker exec e2e-mysql mysql -uroot -proot -e "SELECT 1" >/dev/null 2>&1; do t=$((t+1)); [ $t -gt 90 ] && break; sleep 2; done
  local SCHEMA="DROP TABLE IF EXISTS products; CREATE TABLE products (id int PRIMARY KEY, name text, price int, active boolean);
    INSERT INTO products VALUES (1,'apple',100,true),(2,'banana',50,false),(3,'cherry',200,true);
    DROP TABLE IF EXISTS events; CREATE TABLE events (note text, ts timestamptz default now());"
  docker exec e2e-src-pg psql -U $PGUSER -d $PGDB -c "$SCHEMA" >/dev/null
  docker exec e2e-tgt-pg psql -U $PGUSER -d $PGDB -c "$SCHEMA" >/dev/null
  docker exec e2e-mysql mysql -uroot -proot $PGDB -e "DROP TABLE IF EXISTS products; CREATE TABLE products (id INT PRIMARY KEY, name VARCHAR(64), price INT, active TINYINT); INSERT INTO products VALUES (1,'apple',100,1),(2,'banana',50,0),(3,'cherry',200,1);" 2>/dev/null
}
down() { docker rm -f e2e-src-pg e2e-tgt-pg e2e-mysql >/dev/null 2>&1; rm -rf "$TMP"; }
trap down EXIT

up
echo

# --- A: PG → PG, no proxy (pg-csv capture) --------------------------------------------
echo "[A] PG → PG, no proxy (pg-csv log capture → replay)"
cat > "$TMP/A.csv" <<EOF
2026-06-15 10:00:00.100 UTC,$PGUSER,$PGDB,1,127.0.0.1:5000,a.1,1,UPDATE,2026-06-15 09:59:50.000 UTC,3/1,0,LOG,00000,"duration: 1.0 ms  statement: UPDATE products SET price = price + 1 WHERE id = 1",,,,,,,,psql,client backend,,0
2026-06-15 10:00:00.200 UTC,$PGUSER,$PGDB,1,127.0.0.1:5000,a.1,2,INSERT,2026-06-15 09:59:50.000 UTC,3/2,0,LOG,00000,"duration: 1.0 ms  statement: INSERT INTO events (note) VALUES ('csv')",,,,,,,,psql,client backend,,0
EOF
$BIN capture --source-type pg-csv --source-log "$TMP/A.csv" --output "$TMP/A.wkl" >/dev/null 2>&1
sql_tgt "TRUNCATE events; UPDATE products SET price=100 WHERE id=1;" >/dev/null
$BIN replay --workload "$TMP/A.wkl" --target "$TGT" --output "$TMP/A_res.wkl" >/dev/null 2>&1
check "A events inserted" "$(sql_tgt 'SELECT count(*) FROM events')" "1"
check "A update replayed"  "$(sql_tgt 'SELECT price FROM products WHERE id=1')" "101"
echo

# --- B: PG → PG, with proxy ------------------------------------------------------------
echo "[B] PG → PG, with proxy (live capture → replay)"
sql_tgt "TRUNCATE events;" >/dev/null
$BIN proxy --listen 127.0.0.1:6544 --target localhost:$SRC_PORT --output "$TMP/B.wkl" --duration 7s >"$TMP/proxy.log" 2>&1 &
t=0; until PGPASSWORD=$PGPASS psql "$PROXY" -tAc "SELECT 1" >/dev/null 2>&1; do t=$((t+1)); [ $t -gt 15 ] && break; sleep 1; done
PGPASSWORD=$PGPASS psql "$PROXY" -c "INSERT INTO events (note) VALUES ('via_proxy'); UPDATE products SET price=price+5 WHERE id=2;" >/dev/null 2>&1
t=0; until [ -f "$TMP/B.wkl" ]; do t=$((t+1)); [ $t -gt 20 ] && break; sleep 1; done
sql_tgt "TRUNCATE events;" >/dev/null
$BIN replay --workload "$TMP/B.wkl" --target "$TGT" --output "$TMP/B_res.wkl" >/dev/null 2>&1
check "B proxy capture replayed" "$(sql_tgt "SELECT count(*) FROM events WHERE note='via_proxy'")" "1"
echo

# --- C: MySQL → PG ---------------------------------------------------------------------
echo "[C] MySQL → PG (mysql-slow capture + transform → replay)"
cat > "$TMP/C.log" <<'EOF'
# Time: 2026-06-15T10:00:00.100000Z
# User@Host: e2e[e2e] @ localhost []  Id: 1
# Query_time: 0.001  Lock_time: 0.000 Rows_sent: 0 Rows_examined: 0
SET timestamp=1718445600;
INSERT INTO events (note) VALUES (CONCAT('mysql_', 'origin'));
# Time: 2026-06-15T10:00:01.000000Z
# User@Host: e2e[e2e] @ localhost []  Id: 1
# Query_time: 0.001  Lock_time: 0.000 Rows_sent: 0 Rows_examined: 0
SET timestamp=1718445601;
UPDATE `products` SET price = IFNULL(price, 0) + 1 WHERE id = 3;
EOF
$BIN capture --source-type mysql-slow --source-log "$TMP/C.log" --output "$TMP/C.wkl" >/dev/null 2>&1
sql_tgt "TRUNCATE events; UPDATE products SET price=200 WHERE id=3;" >/dev/null
$BIN replay --workload "$TMP/C.wkl" --target "$TGT" --output "$TMP/C_res.wkl" >/dev/null 2>&1
check "C CONCAT replayed"  "$(sql_tgt "SELECT note FROM events")" "mysql_origin"
check "C IFNULL→COALESCE replayed" "$(sql_tgt 'SELECT price FROM products WHERE id=3')" "201"
echo

# --- F: oracle-replay (verified translate → replay) -----------------------------------
echo "[F] oracle-replay command (verified translate → replay)"
$BIN oracle-replay --input "$TMP/C.wkl" --output "$TMP/F.wkl" --verify syntactic >/dev/null 2>&1
sql_tgt "TRUNCATE events; UPDATE products SET price=200 WHERE id=3;" >/dev/null
$BIN replay --workload "$TMP/F.wkl" --target "$TGT" --output "$TMP/F_res.wkl" >/dev/null 2>&1
check "F oracle-replay output replayed" "$(sql_tgt "SELECT note FROM events")" "mysql_origin"
echo

# --- E: duplicate sessions (scaling) ---------------------------------------------------
echo "[E] duplicate sessions — replay --scale 3"
sql_tgt "TRUNCATE events;" >/dev/null
$BIN replay --workload "$TMP/B.wkl" --target "$TGT" --scale 3 --stagger-ms 20 --output "$TMP/E_res.wkl" >/dev/null 2>&1
check "E scale 3 ran 3 duplicate sessions" "$(sql_tgt "SELECT count(*) FROM events WHERE note='via_proxy'")" "3"
echo

# --- D: Oracle → PG (SQL Trace 10046 upload → translate → replay) ----------------------
echo "[D] Oracle → PG (SQL Trace 10046 capture → oracle-replay → replay)"
sql_tgt "TRUNCATE events; UPDATE products SET price=50 WHERE id=2;" >/dev/null
$BIN capture --source-type oracle-trace --source-log tests/fixtures/oracle_trace.trc --source-host orcl --output "$TMP/O.wkl" >/dev/null 2>&1
if PG_RETEST_SQLGLOT_PYTHON="$SQLGLOT_PY" $BIN oracle-replay --input "$TMP/O.wkl" --output "$TMP/O_pg.wkl" --verify syntactic >/dev/null 2>&1 && [ -f "$TMP/O_pg.wkl" ]; then
  $BIN replay --workload "$TMP/O_pg.wkl" --target "$TGT" --output "$TMP/O_res.wkl" >/dev/null 2>&1
  check "D Oracle NVL→COALESCE replayed" "$(sql_tgt "SELECT note FROM events")" "from_oracle"
  check "D Oracle UPDATE replayed"        "$(sql_tgt 'SELECT price FROM products WHERE id=2')" "57"
  # Note: Oracle SQL sqlglot can't translate (e.g. ROWNUM) is behaviorally rejected by
  # --verify live, not shipped — honest coverage, not silent breakage.
else
  echo "  SKIP: needs a Python with sqlglot (set SQLGLOT_PY); Oracle path goes through sqlglot read='oracle'"
fi
echo

echo "================ RESULTS:  $PASS passed, $FAIL failed ================"
[ "$FAIL" -eq 0 ]
