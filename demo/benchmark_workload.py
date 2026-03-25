#!/usr/bin/env python3
"""
Benchmark workload generator for pg-retest ID correlation testing.

Generates realistic e-commerce traffic with:
- SERIAL sequences (customers, orders, order_items)
- UUID primary keys (tracking_events)
- IDENTITY columns (audit_log, notifications)
- Cross-session ID dependencies (shared_carts)
- NO explicit RETURNING clauses (tests --id-capture-implicit)
- Mixed read/write patterns across 20 concurrent threads

Usage:
    python3 demo/benchmark_workload.py \
        --db-host localhost --db-port 5450 \
        --proxy-host 127.0.0.1 --proxy-port 15433 \
        --threads 20 --warmup 45 --capture 120 --cooldown 45

Flow:
    1. Apply benchmark schema to target DB
    2. Start 20 worker threads hitting the DB directly (warmup, 45s)
    3. Switch all threads to proxy (capture phase, 120s)
    4. Switch back to direct DB (cooldown, 45s)
    5. Print summary stats
"""

import argparse
import os
import random
import string
import subprocess
import sys
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass, field

try:
    import psycopg2
    import psycopg2.pool
except ImportError:
    print("ERROR: psycopg2 required. Install with: pip install psycopg2-binary")
    sys.exit(1)


@dataclass
class Stats:
    inserts: int = 0
    selects: int = 0
    updates: int = 0
    deletes: int = 0
    errors: int = 0
    cross_session_refs: int = 0
    uuid_ops: int = 0
    identity_ops: int = 0
    lock: threading.Lock = field(default_factory=threading.Lock)

    def record(self, op_type, **kwargs):
        with self.lock:
            if op_type == "insert":
                self.inserts += 1
            elif op_type == "select":
                self.selects += 1
            elif op_type == "update":
                self.updates += 1
            elif op_type == "delete":
                self.deletes += 1
            if kwargs.get("error"):
                self.errors += 1
            if kwargs.get("cross_session"):
                self.cross_session_refs += 1
            if kwargs.get("uuid"):
                self.uuid_ops += 1
            if kwargs.get("identity"):
                self.identity_ops += 1

    def summary(self):
        total = self.inserts + self.selects + self.updates + self.deletes
        return (
            f"  Total operations: {total}\n"
            f"  Inserts: {self.inserts}  Selects: {self.selects}  "
            f"Updates: {self.updates}  Deletes: {self.deletes}\n"
            f"  UUID ops: {self.uuid_ops}  Identity ops: {self.identity_ops}  "
            f"Cross-session refs: {self.cross_session_refs}\n"
            f"  Errors: {self.errors}"
        )


# Shared state for cross-session ID passing
# Thread A inserts a shared_cart and puts the ID here
# Thread B picks it up and adds items to it
shared_cart_ids = []
shared_cart_lock = threading.Lock()

# Recently created customer IDs for cross-session order creation
recent_customer_ids = []
recent_customer_lock = threading.Lock()

# Recently created order IDs for cross-session tracking events
recent_order_ids = []
recent_order_lock = threading.Lock()


def random_email():
    """Generate a unique random email."""
    chars = "".join(random.choices(string.ascii_lowercase + string.digits, k=12))
    return f"bench_{chars}@test.com"


def random_string(n=10):
    return "".join(random.choices(string.ascii_letters, k=n))


def get_connection(host, port, dbname="ecommerce", user="demo", password="demo"):
    """Create a new DB connection."""
    return psycopg2.connect(
        host=host, port=port, dbname=dbname, user=user, password=password
    )


def apply_benchmark_schema(host, port):
    """Apply the benchmark schema extension to the database."""
    schema_path = os.path.join(os.path.dirname(__file__), "init-benchmark.sql")
    if not os.path.exists(schema_path):
        print(f"WARNING: {schema_path} not found, skipping schema setup")
        return False

    try:
        conn = get_connection(host, port)
        conn.autocommit = True
        cur = conn.cursor()
        with open(schema_path) as f:
            sql = f.read()
        cur.execute(sql)
        cur.close()
        conn.close()
        print(f"  Benchmark schema applied to {host}:{port}")
        return True
    except psycopg2.errors.DuplicateTable:
        print(f"  Benchmark schema already exists on {host}:{port}")
        return True
    except Exception as e:
        print(f"  WARNING: Schema setup failed: {e}")
        return False


# ─── Workload Operations ──────────────────────────────────────────

def op_insert_customer(conn, stats):
    """INSERT customer (SERIAL PK, no RETURNING)."""
    cur = conn.cursor()
    name = f"Bench_{random_string(8)}"
    email = random_email()
    cur.execute(
        "INSERT INTO customers (name, email) VALUES (%s, %s)",
        (name, email),
    )
    # Use currval to get the ID (no RETURNING!)
    cur.execute("SELECT currval('customers_id_seq')")
    cid = cur.fetchone()[0]
    conn.commit()
    cur.close()

    # Share the ID for cross-session references
    with recent_customer_lock:
        recent_customer_ids.append(cid)
        if len(recent_customer_ids) > 200:
            recent_customer_ids.pop(0)

    stats.record("insert")
    return cid


def op_insert_order_cross_session(conn, stats):
    """INSERT order using a customer ID created by ANOTHER session."""
    with recent_customer_lock:
        if not recent_customer_ids:
            return None
        cid = random.choice(recent_customer_ids)

    cur = conn.cursor()
    total = round(random.uniform(10, 500), 2)
    cur.execute(
        "INSERT INTO orders (customer_id, total, status) VALUES (%s, %s, 'pending')",
        (cid, total),
    )
    cur.execute("SELECT currval('orders_id_seq')")
    oid = cur.fetchone()[0]
    conn.commit()
    cur.close()

    with recent_order_lock:
        recent_order_ids.append(oid)
        if len(recent_order_ids) > 200:
            recent_order_ids.pop(0)

    stats.record("insert", cross_session=True)
    return oid


def op_insert_order_items(conn, stats):
    """INSERT order items for a recently created order (cross-session FK chain)."""
    with recent_order_lock:
        if not recent_order_ids:
            return
        oid = random.choice(recent_order_ids)

    cur = conn.cursor()
    num_items = random.randint(1, 5)
    for _ in range(num_items):
        pid = random.randint(1, 1000)
        qty = random.randint(1, 5)
        price = round(random.uniform(5, 100), 2)
        cur.execute(
            "INSERT INTO order_items (order_id, product_id, qty, price) "
            "VALUES (%s, %s, %s, %s)",
            (oid, pid, qty, price),
        )
    conn.commit()
    cur.close()
    stats.record("insert", cross_session=True)


def op_insert_tracking_event(conn, stats):
    """INSERT tracking_event with UUID PK (no RETURNING, UUID auto-generated)."""
    with recent_order_lock:
        if not recent_order_ids:
            oid = random.randint(1, 20000)
        else:
            oid = random.choice(recent_order_ids)

    cur = conn.cursor()
    event_type = random.choice(["created", "paid", "shipped", "delivered", "returned"])
    cur.execute(
        "INSERT INTO tracking_events (order_id, event_type, payload) "
        "VALUES (%s, %s, %s)",
        (oid, event_type, f'{{"source": "benchmark", "ts": {time.time()}}}'),
    )
    conn.commit()
    cur.close()
    stats.record("insert", uuid=True)


def op_insert_audit_log(conn, stats):
    """INSERT audit_log with IDENTITY column (no RETURNING)."""
    cur = conn.cursor()
    table = random.choice(["customers", "orders", "products", "tracking_events"])
    operation = random.choice(["INSERT", "UPDATE", "DELETE"])
    record_id = str(random.randint(1, 50000))
    cur.execute(
        "INSERT INTO audit_log (table_name, operation, record_id, new_values) "
        "VALUES (%s, %s, %s, %s)",
        (table, operation, record_id, f'{{"bench": true}}'),
    )
    conn.commit()
    cur.close()
    stats.record("insert", identity=True)


def op_create_shared_cart(conn, stats):
    """Create a shared cart (cross-session: other threads will add items)."""
    cur = conn.cursor()
    cid = random.randint(1, 5000)
    cur.execute(
        "INSERT INTO shared_carts (owner_customer_id) VALUES (%s)", (cid,)
    )
    cur.execute("SELECT currval('shared_carts_id_seq')")
    cart_id = cur.fetchone()[0]
    conn.commit()
    cur.close()

    with shared_cart_lock:
        shared_cart_ids.append(cart_id)
        if len(shared_cart_ids) > 100:
            shared_cart_ids.pop(0)

    stats.record("insert", cross_session=True)
    return cart_id


def op_add_to_shared_cart(conn, stats):
    """Add item to a shared cart created by ANOTHER session."""
    with shared_cart_lock:
        if not shared_cart_ids:
            return
        cart_id = random.choice(shared_cart_ids)

    cur = conn.cursor()
    pid = random.randint(1, 1000)
    cid = random.randint(1, 5000)
    qty = random.randint(1, 3)
    cur.execute(
        "INSERT INTO shared_cart_items (cart_id, product_id, added_by_customer_id, qty) "
        "VALUES (%s, %s, %s, %s)",
        (cart_id, pid, cid, qty),
    )
    conn.commit()
    cur.close()
    stats.record("insert", cross_session=True)


def op_insert_notification(conn, stats):
    """INSERT notification with GENERATED BY DEFAULT AS IDENTITY."""
    cur = conn.cursor()
    cid = random.randint(1, 5000)
    channel = random.choice(["email", "sms", "push"])
    cur.execute(
        "INSERT INTO notifications (customer_id, channel, subject, body) "
        "VALUES (%s, %s, %s, %s)",
        (cid, channel, f"Bench notification {random_string(6)}", "Test body"),
    )
    conn.commit()
    cur.close()
    stats.record("insert", identity=True)


def op_select_orders_join(conn, stats):
    """Complex SELECT with JOIN (read workload)."""
    cur = conn.cursor()
    cid = random.randint(1, 5000)
    cur.execute(
        "SELECT o.id, o.total, c.name, COUNT(oi.id) as item_count "
        "FROM orders o "
        "JOIN customers c ON c.id = o.customer_id "
        "LEFT JOIN order_items oi ON oi.order_id = o.id "
        "WHERE o.customer_id = %s "
        "GROUP BY o.id, o.total, c.name "
        "ORDER BY o.created_at DESC LIMIT 10",
        (cid,),
    )
    cur.fetchall()
    cur.close()
    stats.record("select")


def op_select_tracking_by_order(conn, stats):
    """SELECT tracking events by order (UUID table read)."""
    with recent_order_lock:
        if not recent_order_ids:
            oid = random.randint(1, 20000)
        else:
            oid = random.choice(recent_order_ids)

    cur = conn.cursor()
    cur.execute(
        "SELECT id, event_type, payload, created_at "
        "FROM tracking_events WHERE order_id = %s ORDER BY created_at",
        (oid,),
    )
    cur.fetchall()
    cur.close()
    stats.record("select", uuid=True)


def op_select_audit_log(conn, stats):
    """SELECT from audit log (IDENTITY table read)."""
    cur = conn.cursor()
    table = random.choice(["customers", "orders", "products"])
    cur.execute(
        "SELECT id, operation, record_id, performed_at "
        "FROM audit_log WHERE table_name = %s ORDER BY id DESC LIMIT 20",
        (table,),
    )
    cur.fetchall()
    cur.close()
    stats.record("select", identity=True)


def op_update_order_status(conn, stats):
    """UPDATE order status."""
    with recent_order_lock:
        if not recent_order_ids:
            return
        oid = random.choice(recent_order_ids)

    cur = conn.cursor()
    new_status = random.choice(["shipped", "delivered", "cancelled"])
    cur.execute(
        "UPDATE orders SET status = %s WHERE id = %s", (new_status, oid)
    )
    conn.commit()
    cur.close()
    stats.record("update", cross_session=True)


def op_update_product_stock(conn, stats):
    """UPDATE product stock (high-contention)."""
    cur = conn.cursor()
    pid = random.randint(1, 1000)
    delta = random.randint(-5, 10)
    cur.execute(
        "UPDATE products SET stock = GREATEST(0, stock + %s) WHERE id = %s",
        (delta, pid),
    )
    conn.commit()
    cur.close()
    stats.record("update")


# Weighted operation distribution
OPERATIONS = [
    (op_insert_customer, 8),
    (op_insert_order_cross_session, 10),
    (op_insert_order_items, 8),
    (op_insert_tracking_event, 10),
    (op_insert_audit_log, 5),
    (op_create_shared_cart, 3),
    (op_add_to_shared_cart, 5),
    (op_insert_notification, 5),
    (op_select_orders_join, 15),
    (op_select_tracking_by_order, 10),
    (op_select_audit_log, 5),
    (op_update_order_status, 8),
    (op_update_product_stock, 8),
]

# Build weighted list
WEIGHTED_OPS = []
for op, weight in OPERATIONS:
    WEIGHTED_OPS.extend([op] * weight)


def worker_thread(thread_id, host, port, stats, stop_event, phase_name):
    """Worker thread: continuously executes random operations."""
    conn = None
    ops_done = 0
    reconnects = 0

    while not stop_event.is_set():
        try:
            if conn is None or conn.closed:
                conn = get_connection(host, port)
                conn.autocommit = False
                reconnects += 1

            op = random.choice(WEIGHTED_OPS)
            op(conn, stats)
            ops_done += 1

            # Simulate realistic think time (5-50ms)
            time.sleep(random.uniform(0.005, 0.05))

            # Occasionally close and reconnect (simulates connection cycling)
            if random.random() < 0.02:  # 2% chance per op
                conn.close()
                conn = None

        except psycopg2.errors.UniqueViolation:
            if conn and not conn.closed:
                conn.rollback()
            stats.record("insert", error=True)
        except psycopg2.errors.ForeignKeyViolation:
            if conn and not conn.closed:
                conn.rollback()
            stats.record("insert", error=True)
        except (psycopg2.OperationalError, psycopg2.InterfaceError) as e:
            # Connection dropped (proxy shutdown, etc.)
            conn = None
            if not stop_event.is_set():
                time.sleep(0.1)
        except Exception as e:
            if conn and not conn.closed:
                try:
                    conn.rollback()
                except Exception:
                    conn = None
            stats.record("select", error=True)

    if conn and not conn.closed:
        conn.close()


def run_phase(name, host, port, threads, duration, stats):
    """Run a workload phase with the given number of threads."""
    print(f"\n  [{name}] Starting {threads} threads against {host}:{port} for {duration}s...")
    stop = threading.Event()
    workers = []

    for i in range(threads):
        t = threading.Thread(
            target=worker_thread,
            args=(i, host, port, stats, stop, name),
            daemon=True,
        )
        t.start()
        workers.append(t)

    time.sleep(duration)
    stop.set()

    for t in workers:
        t.join(timeout=5)

    print(f"  [{name}] Complete.")
    print(stats.summary())


def main():
    parser = argparse.ArgumentParser(description="Benchmark workload generator for pg-retest")
    parser.add_argument("--db-host", default="localhost", help="Direct DB host")
    parser.add_argument("--db-port", type=int, default=5450, help="Direct DB port")
    parser.add_argument("--proxy-host", default="127.0.0.1", help="Proxy host")
    parser.add_argument("--proxy-port", type=int, default=15433, help="Proxy port")
    parser.add_argument("--threads", type=int, default=20, help="Concurrent threads")
    parser.add_argument("--warmup", type=int, default=45, help="Warmup duration (seconds)")
    parser.add_argument("--capture", type=int, default=120, help="Capture duration (seconds)")
    parser.add_argument("--cooldown", type=int, default=45, help="Cooldown duration (seconds)")
    parser.add_argument("--skip-schema", action="store_true", help="Skip schema setup")
    parser.add_argument("--capture-only", action="store_true", help="Skip warmup/cooldown, only capture phase")
    args = parser.parse_args()

    print("=" * 60)
    print("pg-retest Benchmark Workload Generator")
    print("=" * 60)

    # Step 1: Apply benchmark schema
    if not args.skip_schema:
        print("\nStep 1: Apply benchmark schema...")
        apply_benchmark_schema(args.db_host, args.db_port)
    else:
        print("\nStep 1: Skipping schema setup (--skip-schema)")

    warmup_stats = Stats()
    capture_stats = Stats()
    cooldown_stats = Stats()

    if not args.capture_only:
        # Step 2: Warmup — hit DB directly
        print(f"\nStep 2: Warmup phase ({args.warmup}s, {args.threads} threads, direct DB)")
        print("  This generates data state before capture begins.")
        run_phase("WARMUP", args.db_host, args.db_port, args.threads, args.warmup, warmup_stats)

    # Step 3: Capture — hit proxy
    print(f"\nStep 3: Capture phase ({args.capture}s, {args.threads} threads, via PROXY)")
    print(f"  Ensure pg-retest proxy is running on {args.proxy_host}:{args.proxy_port}")
    print("  Traffic flows: app -> proxy -> db-a")
    run_phase("CAPTURE", args.proxy_host, args.proxy_port, args.threads, args.capture, capture_stats)

    if not args.capture_only:
        # Step 4: Cooldown — hit DB directly
        print(f"\nStep 4: Cooldown phase ({args.cooldown}s, {args.threads} threads, direct DB)")
        run_phase("COOLDOWN", args.db_host, args.db_port, args.threads, args.cooldown, cooldown_stats)

    # Summary
    print("\n" + "=" * 60)
    print("BENCHMARK COMPLETE")
    print("=" * 60)
    if not args.capture_only:
        print(f"\nWarmup stats:\n{warmup_stats.summary()}")
    print(f"\nCapture stats (these are the ops in the .wkl):\n{capture_stats.summary()}")
    if not args.capture_only:
        print(f"\nCooldown stats:\n{cooldown_stats.summary()}")

    print("\nNext steps:")
    print("  1. Stop the proxy (it will write the .wkl file)")
    print("  2. pg_dump db-a > backup.sql")
    print("  3. psql db-b < backup.sql  (restore to same state as capture start)")
    print("  4. pg-retest replay --workload workload.wkl --target db-b --id-mode full")
    print("  5. pg-retest compare --source workload.wkl --replay results.wkl")


if __name__ == "__main__":
    main()
