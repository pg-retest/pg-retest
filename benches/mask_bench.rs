use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pg_retest::capture::masking::mask_sql_literals;

fn bench_mask_simple(c: &mut Criterion) {
    let sql = "SELECT * FROM users WHERE id = 42 AND name = 'Alice'";
    c.bench_function("mask_simple", |b| {
        b.iter(|| mask_sql_literals(black_box(sql)))
    });
}

fn bench_mask_many_literals(c: &mut Criterion) {
    let sql = "INSERT INTO orders (id, customer, price, status, note) VALUES \
               (1, 'Alice', 19.99, 'shipped', 'first order'), \
               (2, 'Bob', 42.50, 'pending', 'it''s urgent'), \
               (3, 'Carol', 7.25, 'cancelled', 'refund requested')";
    c.bench_function("mask_many_literals", |b| {
        b.iter(|| mask_sql_literals(black_box(sql)))
    });
}

fn bench_mask_no_literals(c: &mut Criterion) {
    let sql = "SELECT u.id, u.name, o.total FROM users u \
               INNER JOIN orders o ON o.user_id = u.id \
               WHERE u.active AND o.status = 'paid'";
    c.bench_function("mask_no_literals", |b| {
        b.iter(|| mask_sql_literals(black_box(sql)))
    });
}

fn bench_mask_dollar_quoted(c: &mut Criterion) {
    let sql = "SELECT $tag$hello 'world' with ''quotes''$tag$ FROM t";
    c.bench_function("mask_dollar_quoted", |b| {
        b.iter(|| mask_sql_literals(black_box(sql)))
    });
}

criterion_group!(
    benches,
    bench_mask_simple,
    bench_mask_many_literals,
    bench_mask_no_literals,
    bench_mask_dollar_quoted,
);
criterion_main!(benches);
