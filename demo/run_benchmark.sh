#!/bin/bash
# Full ID correlation benchmark: schema → warmup → capture → backup → restore → replay → compare
set -e

BIN="./target/release/pg-retest"
DB_A="host=localhost port=5450 dbname=ecommerce user=demo password=demo"
DB_B="host=localhost port=5451 dbname=ecommerce user=demo password=demo"
PROXY_PORT=15433
WORKDIR="/tmp/id_benchmark_$(date +%s)"
mkdir -p "$WORKDIR"

echo "═══════════════════════════════════════════════════════════"
echo "  pg-retest ID Correlation Benchmark"
echo "  Workdir: $WORKDIR"
echo "═══════════════════════════════════════════════════════════"

# ─── Step 1: Apply benchmark schema ────────────────────────────
echo ""
echo "Step 1: Apply benchmark schema to db-a..."
psql "$DB_A" -f demo/init-benchmark.sql 2>/dev/null || echo "  (schema already exists)"

# ─── Step 2: Take backup BEFORE warmup ────────────────────────
echo ""
echo "Step 2: Backup db-a state (point-in-time)..."
pg_dump "$DB_A" --clean --if-exists -Fc -f "$WORKDIR/db-a-backup.dump" 2>/dev/null
echo "  Backup: $WORKDIR/db-a-backup.dump ($(du -h "$WORKDIR/db-a-backup.dump" | cut -f1))"

# ─── Step 3: Warmup (direct to db-a, 20s) ─────────────────────
echo ""
echo "Step 3: Warmup — 20 threads hitting db-a directly for 20s..."
python3 demo/benchmark_workload.py \
  --db-host localhost --db-port 5450 \
  --proxy-host 127.0.0.1 --proxy-port $PROXY_PORT \
  --threads 20 --warmup 20 --capture 0 --cooldown 0 \
  --skip-schema --capture-only 2>&1 | grep -E "Total|Inserts|UUID|Error|Complete"

# ─── Step 4: Start proxy + capture (2 min) ─────────────────────
echo ""
echo "Step 4: Start proxy with --id-mode=full --id-capture-implicit..."
$BIN proxy \
  --listen 127.0.0.1:$PROXY_PORT \
  --target "localhost:5450" \
  --id-mode full \
  --source-db "$DB_A" \
  --id-capture-implicit \
  -o "$WORKDIR/workload.wkl" \
  --duration 130s \
  > "$WORKDIR/proxy.log" 2>&1 &
PROXY_PID=$!
sleep 3
echo "  Proxy PID=$PROXY_PID, waiting for it to be ready..."
grep "Listening\|listening\|Discovered" "$WORKDIR/proxy.log" || true

echo ""
echo "Step 5: Capture — 20 threads hitting proxy for 120s..."
python3 demo/benchmark_workload.py \
  --db-host localhost --db-port 5450 \
  --proxy-host 127.0.0.1 --proxy-port $PROXY_PORT \
  --threads 20 --warmup 0 --capture 120 --cooldown 0 \
  --skip-schema --capture-only 2>&1 | grep -E "Total|Inserts|UUID|Error|Complete|Cross"

echo ""
echo "Step 6: Waiting for proxy to flush workload file..."
wait $PROXY_PID 2>/dev/null || true
sleep 2

if [ ! -f "$WORKDIR/workload.wkl" ]; then
  echo "  ERROR: workload file not written!"
  tail -10 "$WORKDIR/proxy.log"
  exit 1
fi
echo "  Workload: $WORKDIR/workload.wkl ($(du -h "$WORKDIR/workload.wkl" | cut -f1))"

echo ""
echo "Step 7: Inspect workload..."
$BIN inspect "$WORKDIR/workload.wkl" 2>&1 | head -8

# ─── Step 8: Restore backup to db-b ───────────────────────────
echo ""
echo "Step 8: Restore backup to db-b (point-in-time restore)..."
pg_restore --clean --if-exists -d "$DB_B" "$WORKDIR/db-a-backup.dump" 2>/dev/null || true
echo "  Restore complete"

# ─── Step 9: Replay with --id-mode=full ────────────────────────
echo ""
echo "Step 9: Replay with --id-mode=full..."
$BIN replay \
  --workload "$WORKDIR/workload.wkl" \
  --target "$DB_B" \
  --id-mode full \
  -o "$WORKDIR/results-full.wkl" 2>&1

# ─── Step 10: Compare ─────────────────────────────────────────
echo ""
echo "Step 10: Compare results..."
$BIN compare \
  --source "$WORKDIR/workload.wkl" \
  --replay "$WORKDIR/results-full.wkl" 2>&1

# ─── Step 11: Replay with --id-mode=none (baseline) ────────────
echo ""
echo "Step 11: Replay with --id-mode=none (baseline, expect more errors)..."
# Re-restore to clean state
pg_restore --clean --if-exists -d "$DB_B" "$WORKDIR/db-a-backup.dump" 2>/dev/null || true
$BIN replay \
  --workload "$WORKDIR/workload.wkl" \
  --target "$DB_B" \
  --id-mode none \
  -o "$WORKDIR/results-none.wkl" 2>&1

echo ""
echo "Step 12: Compare baseline..."
$BIN compare \
  --source "$WORKDIR/workload.wkl" \
  --replay "$WORKDIR/results-none.wkl" 2>&1

echo ""
echo "═══════════════════════════════════════════════════════════"
echo "  BENCHMARK COMPLETE"
echo "  Workdir: $WORKDIR"
echo "═══════════════════════════════════════════════════════════"
