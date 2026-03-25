#!/usr/bin/env python3
"""Python driver test for pg-retest proxy capture."""
import os
import sys
import psycopg2


def main():
    conn_str = os.environ.get(
        "DATABASE_URL",
        "host=localhost port=5433 dbname=ecommerce user=demo password=demo",
    )
    print(f"Connecting to: {conn_str}")

    conn = psycopg2.connect(conn_str)
    conn.autocommit = False
    cur = conn.cursor()

    # 1. Insert customer (no RETURNING)
    cur.execute(
        "INSERT INTO customers (name, email) VALUES (%s, %s)",
        ("PyTest", f"pytest_{os.getpid()}@test.com"),
    )
    cur.execute("SELECT currval('customers_id_seq')")
    customer_id = cur.fetchone()[0]
    print(f"  Created customer: id={customer_id}")

    # 2. Insert order using customer_id (cross-reference)
    cur.execute(
        "INSERT INTO orders (customer_id, total, status) VALUES (%s, %s, %s)",
        (customer_id, 99.99, "pending"),
    )
    cur.execute("SELECT currval('orders_id_seq')")
    order_id = cur.fetchone()[0]
    print(f"  Created order: id={order_id}")

    # 3. Insert order item (FK chain)
    cur.execute(
        "INSERT INTO order_items (order_id, product_id, qty, price) VALUES (%s, %s, %s, %s)",
        (order_id, 1, 2, 49.99),
    )
    print(f"  Created order_item for order {order_id}")

    # 4. Select order back
    cur.execute(
        "SELECT id, customer_id, total, status FROM orders WHERE id = %s", (order_id,)
    )
    row = cur.fetchone()
    assert row is not None, f"Order {order_id} not found!"
    assert row[1] == customer_id, f"Customer ID mismatch: {row[1]} != {customer_id}"
    print(f"  Verified order: id={row[0]}, customer_id={row[1]}, total={row[2]}")

    # 5. Update order status
    cur.execute("UPDATE orders SET status = 'shipped' WHERE id = %s", (order_id,))
    print(f"  Updated order {order_id} to 'shipped'")

    # 6. Insert tracking event (UUID PK)
    cur.execute(
        "INSERT INTO tracking_events (order_id, event_type) VALUES (%s, %s)",
        (order_id, "created"),
    )
    print(f"  Created tracking event for order {order_id}")

    conn.commit()
    cur.close()
    conn.close()
    print("\n  All operations completed successfully!")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        print(f"\n  FAILED: {e}", file=sys.stderr)
        sys.exit(1)
