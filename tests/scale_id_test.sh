#!/bin/bash
# Scale test for ID correlation features
# Requires: db-a on port 5450, db-b on port 5451 (docker compose up db-a db-b)
set -e

BIN="./target/release/pg-retest"
DB_A="host=localhost port=5450 dbname=ecommerce user=demo password=demo"
DB_B="host=localhost port=5451 dbname=ecommerce user=demo password=demo"
PROXY_ADDR="127.0.0.1:15433"
WORKDIR=$(mktemp -d)
trap "rm -rf $WORKDIR; kill %1 2>/dev/null; kill %2 2>/dev/null" EXIT

echo "=== ID Correlation Scale Test ==="
echo "Workdir: $WORKDIR"
echo ""

# ─── Helper: reset db-b sequences to diverge from db-a ───
reset_db_b_sequences() {
    echo "  Advancing db-b sequences by 10000 to force divergence..."
    psql "$DB_B" -q -c "
        SELECT setval('customers_id_seq', (SELECT last_value FROM customers_id_seq) + 10000);
        SELECT setval('products_id_seq', (SELECT last_value FROM products_id_seq) + 10000);
        SELECT setval('orders_id_seq', (SELECT last_value FROM orders_id_seq) + 10000);
        SELECT setval('order_items_id_seq', (SELECT last_value FROM order_items_id_seq) + 10000);
        SELECT setval('reviews_id_seq', (SELECT last_value FROM reviews_id_seq) + 10000);
    " 2>/dev/null
}

# ─── Helper: run a write workload through the proxy ───
generate_write_workload() {
    local count=${1:-50}
    echo "  Generating $count write operations through proxy..."
    for i in $(seq 1 $count); do
        psql "host=127.0.0.1 port=15433 dbname=ecommerce user=demo password=demo" -q -c "
            INSERT INTO customers (name, email) VALUES ('ScaleTest_$i', 'scale_${i}_${RANDOM}@test.com') RETURNING id;
        " 2>/dev/null

        # Get the customer ID and create an order
        psql "host=127.0.0.1 port=15433 dbname=ecommerce user=demo password=demo" -q -c "
            WITH new_order AS (
                INSERT INTO orders (customer_id, total, status)
                VALUES ((SELECT max(id) FROM customers), $((RANDOM % 1000 + 1)).99, 'pending')
                RETURNING id
            )
            INSERT INTO order_items (order_id, product_id, qty, price)
            SELECT new_order.id, (1 + ($RANDOM % 50)), 1 + ($RANDOM % 5), (1 + ($RANDOM % 100))::numeric
            FROM new_order
            RETURNING id;
        " 2>/dev/null
    done
}

# ─── Helper: run a read workload through the proxy ───
generate_read_workload() {
    local count=${1:-100}
    echo "  Generating $count read operations through proxy..."
    for i in $(seq 1 $count); do
        psql "host=127.0.0.1 port=15433 dbname=ecommerce user=demo password=demo" -q -c "
            SELECT o.id, o.total, c.name
            FROM orders o JOIN customers c ON c.id = o.customer_id
            WHERE o.id = (1 + ($RANDOM % 1000))
            LIMIT 10;
        " 2>/dev/null
    done
}

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "TEST 1: Sequence Mode (--id-mode=sequence)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Capture via proxy with sequence snapshot
echo "Step 1: Start proxy with --id-mode=sequence..."
$BIN proxy --listen $PROXY_ADDR --target "localhost:5450" \
    --id-mode sequence --source-db "$DB_A" \
    -o "$WORKDIR/seq_workload.wkl" --duration 15s &
sleep 2

echo "Step 2: Generate mixed workload..."
generate_write_workload 20
generate_read_workload 30
sleep 2
wait %1 2>/dev/null || true

echo "Step 3: Inspect captured workload..."
$BIN inspect "$WORKDIR/seq_workload.wkl" 2>&1 | head -15
echo ""

echo "Step 4: Advance db-b sequences to force divergence..."
reset_db_b_sequences

echo "Step 5: Replay with --id-mode=sequence against db-b..."
$BIN replay --workload "$WORKDIR/seq_workload.wkl" --target "$DB_B" \
    --id-mode sequence -o "$WORKDIR/seq_results.wkl" 2>&1 | tail -10
echo ""

echo "Step 6: Compare..."
$BIN compare --source "$WORKDIR/seq_workload.wkl" --replay "$WORKDIR/seq_results.wkl" 2>&1 | head -20
echo ""
echo "TEST 1 COMPLETE"
echo ""

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "TEST 2: Correlate Mode (--id-mode=correlate)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

echo "Step 1: Start proxy with --id-mode=correlate..."
$BIN proxy --listen $PROXY_ADDR --target "localhost:5450" \
    --id-mode correlate \
    -o "$WORKDIR/corr_workload.wkl" --duration 15s &
sleep 2

echo "Step 2: Generate write workload with RETURNING..."
generate_write_workload 30
sleep 2
wait %1 2>/dev/null || true

echo "Step 3: Inspect..."
$BIN inspect "$WORKDIR/corr_workload.wkl" 2>&1 | head -15
echo ""

echo "Step 4: Advance db-b sequences..."
reset_db_b_sequences

echo "Step 5: Replay with --id-mode=correlate against db-b..."
$BIN replay --workload "$WORKDIR/corr_workload.wkl" --target "$DB_B" \
    --id-mode correlate -o "$WORKDIR/corr_results.wkl" 2>&1 | tail -10
echo ""

echo "Step 6: Compare..."
$BIN compare --source "$WORKDIR/corr_workload.wkl" --replay "$WORKDIR/corr_results.wkl" 2>&1 | head -20
echo ""
echo "TEST 2 COMPLETE"
echo ""

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "TEST 3: Full Mode (--id-mode=full)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

echo "Step 1: Start proxy with --id-mode=full..."
$BIN proxy --listen $PROXY_ADDR --target "localhost:5450" \
    --id-mode full --source-db "$DB_A" \
    -o "$WORKDIR/full_workload.wkl" --duration 15s &
sleep 2

echo "Step 2: Generate heavy write workload..."
generate_write_workload 50
sleep 2
wait %1 2>/dev/null || true

echo "Step 3: Inspect..."
$BIN inspect "$WORKDIR/full_workload.wkl" 2>&1 | head -15
echo ""

echo "Step 4: Advance db-b sequences by 10000..."
reset_db_b_sequences

echo "Step 5: Replay with --id-mode=full against db-b..."
$BIN replay --workload "$WORKDIR/full_workload.wkl" --target "$DB_B" \
    --id-mode full -o "$WORKDIR/full_results.wkl" 2>&1 | tail -10
echo ""

echo "Step 6: Compare..."
$BIN compare --source "$WORKDIR/full_workload.wkl" --replay "$WORKDIR/full_results.wkl" 2>&1 | head -20
echo ""
echo "TEST 3 COMPLETE"
echo ""

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "TEST 4: Full Mode + Scaled Replay (--scale 4)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

echo "Step 1: Reuse full_workload.wkl from Test 3"
echo "Step 2: Advance db-b sequences..."
reset_db_b_sequences

echo "Step 3: Replay with --id-mode=full --scale 4 against db-b..."
$BIN replay --workload "$WORKDIR/full_workload.wkl" --target "$DB_B" \
    --id-mode full --scale 4 --stagger-ms 100 \
    -o "$WORKDIR/scaled_results.wkl" 2>&1 | tail -10
echo ""

echo "Step 4: Compare..."
$BIN compare --source "$WORKDIR/full_workload.wkl" --replay "$WORKDIR/scaled_results.wkl" 2>&1 | head -20
echo ""
echo "TEST 4 COMPLETE"
echo ""

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "TEST 5: None Mode Baseline (--id-mode=none)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

echo "Step 1: Reuse full_workload.wkl, advance sequences..."
reset_db_b_sequences

echo "Step 2: Replay WITHOUT id-mode (baseline — expect errors)..."
$BIN replay --workload "$WORKDIR/full_workload.wkl" --target "$DB_B" \
    --id-mode none -o "$WORKDIR/none_results.wkl" 2>&1 | tail -10
echo ""

echo "Step 3: Compare (expect high error rate — this is the BEFORE)..."
$BIN compare --source "$WORKDIR/full_workload.wkl" --replay "$WORKDIR/none_results.wkl" 2>&1 | head -20
echo ""
echo "TEST 5 COMPLETE (baseline — errors expected)"
echo ""

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "ALL TESTS COMPLETE"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
