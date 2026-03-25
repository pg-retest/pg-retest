package main

import (
	"context"
	"fmt"
	"os"

	"github.com/jackc/pgx/v5"
)

func main() {
	connStr := os.Getenv("DATABASE_URL")
	if connStr == "" {
		connStr = "host=localhost port=5433 dbname=ecommerce user=demo password=demo"
	}
	fmt.Printf("Connecting to: %s\n", connStr)

	ctx := context.Background()
	conn, err := pgx.Connect(ctx, connStr)
	if err != nil {
		fmt.Fprintf(os.Stderr, "\n  FAILED: %v\n", err)
		os.Exit(1)
	}
	defer conn.Close(ctx)

	tx, err := conn.Begin(ctx)
	if err != nil {
		fmt.Fprintf(os.Stderr, "\n  FAILED: %v\n", err)
		os.Exit(1)
	}
	defer tx.Rollback(ctx)

	// 1. Insert customer (no RETURNING)
	pid := os.Getpid()
	_, err = tx.Exec(ctx,
		"INSERT INTO customers (name, email) VALUES ($1, $2)",
		"GoTest", fmt.Sprintf("gotest_%d@test.com", pid))
	if err != nil {
		fmt.Fprintf(os.Stderr, "\n  FAILED: insert customer: %v\n", err)
		os.Exit(1)
	}

	var customerID int64
	err = tx.QueryRow(ctx, "SELECT currval('customers_id_seq')").Scan(&customerID)
	if err != nil {
		fmt.Fprintf(os.Stderr, "\n  FAILED: currval customers: %v\n", err)
		os.Exit(1)
	}
	fmt.Printf("  Created customer: id=%d\n", customerID)

	// 2. Insert order using customer_id (cross-reference)
	_, err = tx.Exec(ctx,
		"INSERT INTO orders (customer_id, total, status) VALUES ($1, $2, $3)",
		customerID, 99.99, "pending")
	if err != nil {
		fmt.Fprintf(os.Stderr, "\n  FAILED: insert order: %v\n", err)
		os.Exit(1)
	}

	var orderID int64
	err = tx.QueryRow(ctx, "SELECT currval('orders_id_seq')").Scan(&orderID)
	if err != nil {
		fmt.Fprintf(os.Stderr, "\n  FAILED: currval orders: %v\n", err)
		os.Exit(1)
	}
	fmt.Printf("  Created order: id=%d\n", orderID)

	// 3. Insert order item (FK chain)
	_, err = tx.Exec(ctx,
		"INSERT INTO order_items (order_id, product_id, qty, price) VALUES ($1, $2, $3, $4)",
		orderID, 1, 2, 49.99)
	if err != nil {
		fmt.Fprintf(os.Stderr, "\n  FAILED: insert order_item: %v\n", err)
		os.Exit(1)
	}
	fmt.Printf("  Created order_item for order %d\n", orderID)

	// 4. Select order back
	var id, custID int64
	var total float64
	var status string
	err = tx.QueryRow(ctx,
		"SELECT id, customer_id, total, status FROM orders WHERE id = $1",
		orderID).Scan(&id, &custID, &total, &status)
	if err != nil {
		fmt.Fprintf(os.Stderr, "\n  FAILED: select order: %v\n", err)
		os.Exit(1)
	}
	if custID != customerID {
		fmt.Fprintf(os.Stderr, "\n  FAILED: customer ID mismatch: %d != %d\n", custID, customerID)
		os.Exit(1)
	}
	fmt.Printf("  Verified order: id=%d, customer_id=%d, total=%.2f\n", id, custID, total)

	// 5. Update order status
	_, err = tx.Exec(ctx, "UPDATE orders SET status = 'shipped' WHERE id = $1", orderID)
	if err != nil {
		fmt.Fprintf(os.Stderr, "\n  FAILED: update order: %v\n", err)
		os.Exit(1)
	}
	fmt.Printf("  Updated order %d to 'shipped'\n", orderID)

	// 6. Insert tracking event (UUID PK)
	_, err = tx.Exec(ctx,
		"INSERT INTO tracking_events (order_id, event_type) VALUES ($1, $2)",
		orderID, "created")
	if err != nil {
		fmt.Fprintf(os.Stderr, "\n  FAILED: insert tracking event: %v\n", err)
		os.Exit(1)
	}
	fmt.Printf("  Created tracking event for order %d\n", orderID)

	err = tx.Commit(ctx)
	if err != nil {
		fmt.Fprintf(os.Stderr, "\n  FAILED: commit: %v\n", err)
		os.Exit(1)
	}

	fmt.Println("\n  All operations completed successfully!")
}
