use pg_retest::capture::masking::mask_sql_literals;

#[test]
fn test_mask_insert_with_mixed_literals() {
    let sql = "INSERT INTO users (name, age, email) VALUES ('John Doe', 30, 'john@example.com')";
    let masked = mask_sql_literals(sql);
    assert_eq!(
        masked,
        "INSERT INTO users (name, age, email) VALUES ($S, $N, $S)"
    );
}

#[test]
fn test_mask_where_clause_string() {
    let sql = "SELECT * FROM users WHERE email = 'alice@corp.com' AND status = 'active'";
    let masked = mask_sql_literals(sql);
    assert_eq!(
        masked,
        "SELECT * FROM users WHERE email = $S AND status = $S"
    );
}

#[test]
fn test_mask_update_with_literals() {
    let sql = "UPDATE accounts SET balance = 1500.75 WHERE account_id = 12345";
    let masked = mask_sql_literals(sql);
    assert_eq!(
        masked,
        "UPDATE accounts SET balance = $N WHERE account_id = $N"
    );
}

#[test]
fn test_mask_preserves_sql_structure() {
    let sql = "SELECT col1, col2 FROM table3 WHERE col1 IS NOT NULL ORDER BY col2 DESC LIMIT 10";
    let masked = mask_sql_literals(sql);
    assert_eq!(
        masked,
        "SELECT col1, col2 FROM table3 WHERE col1 IS NOT NULL ORDER BY col2 DESC LIMIT $N"
    );
}

#[test]
fn test_mask_empty_string_literal() {
    let sql = "SELECT * FROM t WHERE name = ''";
    let masked = mask_sql_literals(sql);
    assert_eq!(masked, "SELECT * FROM t WHERE name = $S");
}

#[test]
fn test_mask_scientific_notation() {
    let sql = "SELECT * FROM t WHERE val > 1.5e10";
    let masked = mask_sql_literals(sql);
    assert_eq!(masked, "SELECT * FROM t WHERE val > $N");
}
