#!/usr/bin/env node
"use strict";

const { Client } = require("pg");

async function main() {
  const connStr =
    process.env.DATABASE_URL ||
    "host=localhost port=5433 dbname=ecommerce user=demo password=demo";
  console.log(`Connecting to: ${connStr}`);

  // Parse libpq-style connection string for node-pg
  const connOpts = {};
  if (connStr.startsWith("postgres://") || connStr.startsWith("postgresql://")) {
    connOpts.connectionString = connStr;
  } else {
    for (const part of connStr.split(/\s+/)) {
      const [key, ...rest] = part.split("=");
      const val = rest.join("=");
      if (key === "host") connOpts.host = val;
      else if (key === "port") connOpts.port = parseInt(val, 10);
      else if (key === "dbname") connOpts.database = val;
      else if (key === "user") connOpts.user = val;
      else if (key === "password") connOpts.password = val;
    }
  }

  const client = new Client(connOpts);
  await client.connect();

  try {
    await client.query("BEGIN");

    // 1. Insert customer (no RETURNING)
    const pid = process.pid;
    await client.query(
      "INSERT INTO customers (name, email) VALUES ($1, $2)",
      ["NodeTest", `nodetest_${pid}@test.com`]
    );
    const custRes = await client.query("SELECT currval('customers_id_seq')");
    const customerId = parseInt(custRes.rows[0].currval, 10);
    console.log(`  Created customer: id=${customerId}`);

    // 2. Insert order using customer_id (cross-reference)
    await client.query(
      "INSERT INTO orders (customer_id, total, status) VALUES ($1, $2, $3)",
      [customerId, 99.99, "pending"]
    );
    const ordRes = await client.query("SELECT currval('orders_id_seq')");
    const orderId = parseInt(ordRes.rows[0].currval, 10);
    console.log(`  Created order: id=${orderId}`);

    // 3. Insert order item (FK chain)
    await client.query(
      "INSERT INTO order_items (order_id, product_id, qty, price) VALUES ($1, $2, $3, $4)",
      [orderId, 1, 2, 49.99]
    );
    console.log(`  Created order_item for order ${orderId}`);

    // 4. Select order back
    const selRes = await client.query(
      "SELECT id, customer_id, total, status FROM orders WHERE id = $1",
      [orderId]
    );
    const row = selRes.rows[0];
    if (!row) throw new Error(`Order ${orderId} not found!`);
    if (parseInt(row.customer_id, 10) !== customerId) {
      throw new Error(
        `Customer ID mismatch: ${row.customer_id} != ${customerId}`
      );
    }
    console.log(
      `  Verified order: id=${row.id}, customer_id=${row.customer_id}, total=${row.total}`
    );

    // 5. Update order status
    await client.query("UPDATE orders SET status = 'shipped' WHERE id = $1", [
      orderId,
    ]);
    console.log(`  Updated order ${orderId} to 'shipped'`);

    // 6. Insert tracking event (UUID PK)
    await client.query(
      "INSERT INTO tracking_events (order_id, event_type) VALUES ($1, $2)",
      [orderId, "created"]
    );
    console.log(`  Created tracking event for order ${orderId}`);

    await client.query("COMMIT");
    console.log("\n  All operations completed successfully!");
  } catch (err) {
    await client.query("ROLLBACK").catch(() => {});
    throw err;
  } finally {
    await client.end();
  }
}

main().catch((err) => {
  console.error(`\n  FAILED: ${err.message}`);
  process.exit(1);
});
