/*
 * C driver test for pg-retest proxy capture.
 * Uses libpq directly.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <libpq-fe.h>

static void die(PGconn *conn, const char *msg) {
    fprintf(stderr, "\n  FAILED: %s: %s\n", msg, PQerrorMessage(conn));
    PQfinish(conn);
    exit(1);
}

static void check_result(PGconn *conn, PGresult *res, ExecStatusType expected, const char *op) {
    if (PQresultStatus(res) != expected) {
        fprintf(stderr, "\n  FAILED: %s: %s\n", op, PQerrorMessage(conn));
        PQclear(res);
        PQfinish(conn);
        exit(1);
    }
}

int main(void) {
    const char *conninfo = getenv("DATABASE_URL");
    if (!conninfo)
        conninfo = "host=localhost port=5433 dbname=ecommerce user=demo password=demo";

    printf("Connecting to: %s\n", conninfo);

    PGconn *conn = PQconnectdb(conninfo);
    if (PQstatus(conn) != CONNECTION_OK)
        die(conn, "connection failed");

    PGresult *res;
    char customer_id_str[32];
    char order_id_str[32];
    char email[64];
    long customer_id, order_id;

    /* BEGIN */
    res = PQexec(conn, "BEGIN");
    check_result(conn, res, PGRES_COMMAND_OK, "BEGIN");
    PQclear(res);

    /* 1. Insert customer (parameterized, no RETURNING) */
    snprintf(email, sizeof(email), "ctest_%d@test.com", getpid());
    const char *ins_cust_params[] = {"CTest", email};
    res = PQexecParams(conn,
        "INSERT INTO customers (name, email) VALUES ($1, $2)",
        2, NULL, ins_cust_params, NULL, NULL, 0);
    check_result(conn, res, PGRES_COMMAND_OK, "insert customer");
    PQclear(res);

    /* Get customer_id via currval */
    res = PQexec(conn, "SELECT currval('customers_id_seq')");
    check_result(conn, res, PGRES_TUPLES_OK, "currval customers");
    customer_id = atol(PQgetvalue(res, 0, 0));
    snprintf(customer_id_str, sizeof(customer_id_str), "%ld", customer_id);
    PQclear(res);
    printf("  Created customer: id=%ld\n", customer_id);

    /* 2. Insert order using customer_id (cross-reference) */
    const char *ins_ord_params[] = {customer_id_str, "99.99", "pending"};
    res = PQexecParams(conn,
        "INSERT INTO orders (customer_id, total, status) VALUES ($1, $2, $3)",
        3, NULL, ins_ord_params, NULL, NULL, 0);
    check_result(conn, res, PGRES_COMMAND_OK, "insert order");
    PQclear(res);

    /* Get order_id via currval */
    res = PQexec(conn, "SELECT currval('orders_id_seq')");
    check_result(conn, res, PGRES_TUPLES_OK, "currval orders");
    order_id = atol(PQgetvalue(res, 0, 0));
    snprintf(order_id_str, sizeof(order_id_str), "%ld", order_id);
    PQclear(res);
    printf("  Created order: id=%ld\n", order_id);

    /* 3. Insert order item (FK chain) */
    const char *ins_item_params[] = {order_id_str, "1", "2", "49.99"};
    res = PQexecParams(conn,
        "INSERT INTO order_items (order_id, product_id, qty, price) VALUES ($1, $2, $3, $4)",
        4, NULL, ins_item_params, NULL, NULL, 0);
    check_result(conn, res, PGRES_COMMAND_OK, "insert order_item");
    PQclear(res);
    printf("  Created order_item for order %ld\n", order_id);

    /* 4. Select order back */
    const char *sel_params[] = {order_id_str};
    res = PQexecParams(conn,
        "SELECT id, customer_id, total, status FROM orders WHERE id = $1",
        1, NULL, sel_params, NULL, NULL, 0);
    check_result(conn, res, PGRES_TUPLES_OK, "select order");
    if (PQntuples(res) == 0) {
        fprintf(stderr, "\n  FAILED: Order %ld not found!\n", order_id);
        PQclear(res);
        PQfinish(conn);
        return 1;
    }
    long fetched_cust = atol(PQgetvalue(res, 0, 1));
    if (fetched_cust != customer_id) {
        fprintf(stderr, "\n  FAILED: Customer ID mismatch: %ld != %ld\n",
                fetched_cust, customer_id);
        PQclear(res);
        PQfinish(conn);
        return 1;
    }
    printf("  Verified order: id=%s, customer_id=%s, total=%s\n",
           PQgetvalue(res, 0, 0), PQgetvalue(res, 0, 1), PQgetvalue(res, 0, 2));
    PQclear(res);

    /* 5. Update order status */
    const char *upd_params[] = {order_id_str};
    res = PQexecParams(conn,
        "UPDATE orders SET status = 'shipped' WHERE id = $1",
        1, NULL, upd_params, NULL, NULL, 0);
    check_result(conn, res, PGRES_COMMAND_OK, "update order");
    PQclear(res);
    printf("  Updated order %ld to 'shipped'\n", order_id);

    /* 6. Insert tracking event (UUID PK) */
    const char *ins_track_params[] = {order_id_str, "created"};
    res = PQexecParams(conn,
        "INSERT INTO tracking_events (order_id, event_type) VALUES ($1, $2)",
        2, NULL, ins_track_params, NULL, NULL, 0);
    check_result(conn, res, PGRES_COMMAND_OK, "insert tracking event");
    PQclear(res);
    printf("  Created tracking event for order %ld\n", order_id);

    /* COMMIT */
    res = PQexec(conn, "COMMIT");
    check_result(conn, res, PGRES_COMMAND_OK, "COMMIT");
    PQclear(res);

    PQfinish(conn);
    printf("\n  All operations completed successfully!\n");
    return 0;
}
