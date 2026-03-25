import java.sql.*;

public class TestDriver {
    public static void main(String[] args) {
        String host = env("PGHOST", "localhost");
        String port = env("PGPORT", "5433");
        String db = env("PGDATABASE", "ecommerce");
        String user = env("PGUSER", "demo");
        String password = env("PGPASSWORD", "demo");

        String url = String.format("jdbc:postgresql://%s:%s/%s", host, port, db);
        System.out.printf("Connecting to: %s (user=%s)%n", url, user);

        try (Connection conn = DriverManager.getConnection(url, user, password)) {
            conn.setAutoCommit(false);

            // 1. Insert customer (no RETURNING)
            long pid = ProcessHandle.current().pid();
            try (PreparedStatement ps = conn.prepareStatement(
                    "INSERT INTO customers (name, email) VALUES (?, ?)")) {
                ps.setString(1, "JavaTest");
                ps.setString(2, "javatest_" + pid + "@test.com");
                ps.executeUpdate();
            }

            long customerId;
            try (Statement st = conn.createStatement();
                 ResultSet rs = st.executeQuery("SELECT currval('customers_id_seq')")) {
                rs.next();
                customerId = rs.getLong(1);
            }
            System.out.printf("  Created customer: id=%d%n", customerId);

            // 2. Insert order using customer_id (cross-reference)
            try (PreparedStatement ps = conn.prepareStatement(
                    "INSERT INTO orders (customer_id, total, status) VALUES (?, ?, ?)")) {
                ps.setLong(1, customerId);
                ps.setDouble(2, 99.99);
                ps.setString(3, "pending");
                ps.executeUpdate();
            }

            long orderId;
            try (Statement st = conn.createStatement();
                 ResultSet rs = st.executeQuery("SELECT currval('orders_id_seq')")) {
                rs.next();
                orderId = rs.getLong(1);
            }
            System.out.printf("  Created order: id=%d%n", orderId);

            // 3. Insert order item (FK chain)
            try (PreparedStatement ps = conn.prepareStatement(
                    "INSERT INTO order_items (order_id, product_id, qty, price) VALUES (?, ?, ?, ?)")) {
                ps.setLong(1, orderId);
                ps.setInt(2, 1);
                ps.setInt(3, 2);
                ps.setDouble(4, 49.99);
                ps.executeUpdate();
            }
            System.out.printf("  Created order_item for order %d%n", orderId);

            // 4. Select order back
            try (PreparedStatement ps = conn.prepareStatement(
                    "SELECT id, customer_id, total, status FROM orders WHERE id = ?")) {
                ps.setLong(1, orderId);
                try (ResultSet rs = ps.executeQuery()) {
                    if (!rs.next()) {
                        throw new RuntimeException("Order " + orderId + " not found!");
                    }
                    long custId = rs.getLong("customer_id");
                    if (custId != customerId) {
                        throw new RuntimeException(
                                "Customer ID mismatch: " + custId + " != " + customerId);
                    }
                    System.out.printf("  Verified order: id=%d, customer_id=%d, total=%.2f%n",
                            rs.getLong("id"), custId, rs.getDouble("total"));
                }
            }

            // 5. Update order status
            try (PreparedStatement ps = conn.prepareStatement(
                    "UPDATE orders SET status = 'shipped' WHERE id = ?")) {
                ps.setLong(1, orderId);
                ps.executeUpdate();
            }
            System.out.printf("  Updated order %d to 'shipped'%n", orderId);

            // 6. Insert tracking event (UUID PK)
            try (PreparedStatement ps = conn.prepareStatement(
                    "INSERT INTO tracking_events (order_id, event_type) VALUES (?, ?)")) {
                ps.setLong(1, orderId);
                ps.setString(2, "created");
                ps.executeUpdate();
            }
            System.out.printf("  Created tracking event for order %d%n", orderId);

            conn.commit();
            System.out.println("\n  All operations completed successfully!");

        } catch (Exception e) {
            System.err.printf("%n  FAILED: %s%n", e.getMessage());
            System.exit(1);
        }
    }

    private static String env(String key, String defaultValue) {
        String val = System.getenv(key);
        return (val != null && !val.isEmpty()) ? val : defaultValue;
    }
}
