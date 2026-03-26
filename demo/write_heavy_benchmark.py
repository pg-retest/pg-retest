#!/usr/bin/env python3
"""
Write-heavy benchmark for pg-retest ID correlation testing.

80% writes (INSERT/UPDATE/DELETE), 20% reads.
All operations use generated IDs — every SELECT references an ID
that was created by a prior INSERT in the SAME session.
Each inserted row gets selected 2-3 times over its lifetime.

Pattern per session:
  INSERT customer → get id via currval → SELECT customer by id
  INSERT order (FK to customer) → get id → SELECT order by id
  UPDATE order status by id → SELECT order again
  INSERT order_items (FK to order) → SELECT order with items
  DELETE old tracking events by order_id
  ... repeat

This tests the hardest case: every query depends on a generated ID.
"""

import argparse
import os
import random
import string
import sys
import threading
import time
from dataclasses import dataclass, field

try:
    import psycopg2
except ImportError:
    print("ERROR: pip install psycopg2-binary")
    sys.exit(1)


@dataclass
class Stats:
    inserts: int = 0
    updates: int = 0
    deletes: int = 0
    selects: int = 0
    errors: int = 0
    lock: threading.Lock = field(default_factory=threading.Lock)

    def inc(self, op, error=False):
        with self.lock:
            if op == "insert": self.inserts += 1
            elif op == "update": self.updates += 1
            elif op == "delete": self.deletes += 1
            elif op == "select": self.selects += 1
            if error: self.errors += 1

    def total(self):
        return self.inserts + self.updates + self.deletes + self.selects

    def summary(self):
        t = self.total()
        wp = (self.inserts + self.updates + self.deletes) / t * 100 if t else 0
        return (
            f"  Total: {t}  Writes: {self.inserts + self.updates + self.deletes} ({wp:.0f}%)  "
            f"Reads: {self.selects} ({100-wp:.0f}%)\n"
            f"  INSERT: {self.inserts}  UPDATE: {self.updates}  DELETE: {self.deletes}  SELECT: {self.selects}\n"
            f"  Errors: {self.errors}"
        )


def rand_email():
    return f"wh_{''.join(random.choices(string.ascii_lowercase, k=10))}_{random.randint(1,99999)}@test.com"


def rand_name():
    return f"WH_{''.join(random.choices(string.ascii_letters, k=8))}"


def worker(thread_id, host, port, stats, stop_event):
    """
    Each worker runs a realistic write-heavy session loop:
    1. INSERT customer (no RETURNING — uses currval)
    2. SELECT that customer back by ID
    3. INSERT order for that customer (no RETURNING — uses currval)
    4. SELECT that order back
    5. INSERT 1-3 order_items for that order
    6. UPDATE order total
    7. SELECT order with items (JOIN)
    8. INSERT tracking event for the order
    9. SELECT tracking events for that order
    10. Maybe DELETE old tracking events
    11. SELECT customer again (2nd read of same ID)

    ~80% writes, ~20% reads, all using generated IDs
    """
    conn = None
    ops = 0

    while not stop_event.is_set():
        try:
            if conn is None or conn.closed:
                conn = psycopg2.connect(
                    host=host, port=port, dbname="ecommerce",
                    user="demo", password="demo"
                )
                conn.autocommit = False

            cur = conn.cursor()

            # === Transaction: Create customer + order + items ===
            cur.execute("BEGIN")

            # 1. INSERT customer (no RETURNING!)
            cur.execute(
                "INSERT INTO customers (name, email) VALUES (%s, %s)",
                (rand_name(), rand_email())
            )
            cur.execute("SELECT currval('customers_id_seq')")
            customer_id = cur.fetchone()[0]
            stats.inc("insert")

            # 2. SELECT customer back by generated ID
            cur.execute(
                "SELECT id, name, email, created_at FROM customers WHERE id = %s",
                (customer_id,)
            )
            row = cur.fetchone()
            stats.inc("select")
            assert row is not None, f"Customer {customer_id} not found after INSERT"

            # 3. INSERT order for this customer (no RETURNING!)
            total = round(random.uniform(10, 500), 2)
            cur.execute(
                "INSERT INTO orders (customer_id, total, status) VALUES (%s, %s, 'pending')",
                (customer_id, total)
            )
            cur.execute("SELECT currval('orders_id_seq')")
            order_id = cur.fetchone()[0]
            stats.inc("insert")

            # 4. SELECT order back by generated ID
            cur.execute(
                "SELECT id, customer_id, total, status FROM orders WHERE id = %s",
                (order_id,)
            )
            row = cur.fetchone()
            stats.inc("select")
            assert row is not None, f"Order {order_id} not found"
            assert row[1] == customer_id, f"FK mismatch: order.customer_id={row[1]} != {customer_id}"

            # 5. INSERT 1-3 order items
            num_items = random.randint(1, 3)
            item_total = 0
            for _ in range(num_items):
                pid = random.randint(1, 1000)
                qty = random.randint(1, 5)
                price = round(random.uniform(5, 100), 2)
                item_total += qty * price
                cur.execute(
                    "INSERT INTO order_items (order_id, product_id, qty, price) VALUES (%s, %s, %s, %s)",
                    (order_id, pid, qty, price)
                )
                stats.inc("insert")

            # 6. UPDATE order total with calculated amount
            cur.execute(
                "UPDATE orders SET total = %s WHERE id = %s",
                (round(item_total, 2), order_id)
            )
            stats.inc("update")

            # 7. SELECT order with items (JOIN query using generated IDs)
            cur.execute("""
                SELECT o.id, o.total, o.status, COUNT(oi.id) as item_count,
                       SUM(oi.qty * oi.price) as items_total
                FROM orders o
                JOIN order_items oi ON oi.order_id = o.id
                WHERE o.id = %s
                GROUP BY o.id, o.total, o.status
            """, (order_id,))
            row = cur.fetchone()
            stats.inc("select")

            # 8. INSERT tracking event (UUID table, FK to order)
            cur.execute(
                "INSERT INTO tracking_events (order_id, event_type, payload) VALUES (%s, %s, %s)",
                (order_id, random.choice(['created', 'processing', 'shipped']),
                 f'{{"thread": {thread_id}, "ops": {ops}}}')
            )
            stats.inc("insert")

            # 9. UPDATE order status
            cur.execute(
                "UPDATE orders SET status = %s WHERE id = %s",
                (random.choice(['processing', 'shipped', 'delivered']), order_id)
            )
            stats.inc("update")

            # 10. SELECT tracking events for this order
            cur.execute("""
                SELECT id, event_type, payload, created_at
                FROM tracking_events
                WHERE order_id = %s
                ORDER BY created_at
            """, (order_id,))
            events = cur.fetchall()
            stats.inc("select")

            # 11. Maybe DELETE old tracking events (20% chance)
            if random.random() < 0.2 and events:
                cur.execute(
                    "DELETE FROM tracking_events WHERE order_id = %s AND event_type = 'created'",
                    (order_id,)
                )
                stats.inc("delete")

            # 12. SELECT customer again (2nd read of same generated ID)
            cur.execute(
                "SELECT id, name, email FROM customers WHERE id = %s",
                (customer_id,)
            )
            stats.inc("select")

            # 13. UPDATE product stock (write to non-generated-id table)
            cur.execute(
                "UPDATE products SET stock = GREATEST(0, stock + %s) WHERE id = %s",
                (random.randint(-3, 5), random.randint(1, 1000))
            )
            stats.inc("update")

            cur.execute("COMMIT")
            ops += 1

            # Think time: 5-30ms
            time.sleep(random.uniform(0.005, 0.03))

            # Reconnect occasionally (10% chance)
            if random.random() < 0.1:
                conn.close()
                conn = None

        except psycopg2.errors.UniqueViolation:
            if conn and not conn.closed:
                conn.rollback()
            stats.inc("insert", error=True)
        except psycopg2.errors.ForeignKeyViolation:
            if conn and not conn.closed:
                conn.rollback()
            stats.inc("insert", error=True)
        except (psycopg2.OperationalError, psycopg2.InterfaceError):
            conn = None
            if not stop_event.is_set():
                time.sleep(0.1)
        except AssertionError as e:
            if conn and not conn.closed:
                conn.rollback()
            stats.inc("select", error=True)
            print(f"  ASSERTION FAILED (thread {thread_id}): {e}", file=sys.stderr)
        except Exception as e:
            if conn and not conn.closed:
                try: conn.rollback()
                except: conn = None
            stats.inc("insert", error=True)

    if conn and not conn.closed:
        conn.close()


def run_phase(name, host, port, threads, duration, stats):
    print(f"\n  [{name}] {threads} threads → {host}:{port} for {duration}s...")
    stop = threading.Event()
    workers = []
    for i in range(threads):
        t = threading.Thread(target=worker, args=(i, host, port, stats, stop), daemon=True)
        t.start()
        workers.append(t)
    time.sleep(duration)
    stop.set()
    for t in workers:
        t.join(timeout=5)
    print(f"  [{name}] Done.")
    print(stats.summary())


def main():
    parser = argparse.ArgumentParser(description="Write-heavy benchmark (80% writes, generated IDs)")
    parser.add_argument("--db-host", default="localhost")
    parser.add_argument("--db-port", type=int, default=5450)
    parser.add_argument("--proxy-host", default="127.0.0.1")
    parser.add_argument("--proxy-port", type=int, default=15433)
    parser.add_argument("--threads", type=int, default=20)
    parser.add_argument("--warmup", type=int, default=15)
    parser.add_argument("--capture", type=int, default=60)
    parser.add_argument("--cooldown", type=int, default=0)
    args = parser.parse_args()

    print("=" * 60)
    print("  Write-Heavy Benchmark (80% writes, generated IDs)")
    print("=" * 60)

    warmup_stats = Stats()
    capture_stats = Stats()

    if args.warmup > 0:
        print(f"\n  Warmup: {args.warmup}s direct to {args.db_host}:{args.db_port}")
        run_phase("WARMUP", args.db_host, args.db_port, args.threads, args.warmup, warmup_stats)

    if args.capture > 0:
        print(f"\n  Capture: {args.capture}s through proxy {args.proxy_host}:{args.proxy_port}")
        run_phase("CAPTURE", args.proxy_host, args.proxy_port, args.threads, args.capture, capture_stats)

    print("\n" + "=" * 60)
    print("  BENCHMARK COMPLETE")
    print("=" * 60)
    if args.warmup > 0:
        print(f"\nWarmup:\n{warmup_stats.summary()}")
    print(f"\nCapture:\n{capture_stats.summary()}")


if __name__ == "__main__":
    main()
