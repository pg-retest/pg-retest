use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pg_retest::correlate::capture::has_returning;

fn bench_returning_select_skipped(c: &mut Criterion) {
    // Should short-circuit via pre-filter (not INSERT/UPDATE/DELETE/MERGE/WITH)
    // once Task 5 lands. Baseline captured against the legacy substring-scan
    // impl which does not have a pre-filter — pre-change numbers here are the
    // legacy cost on a SELECT.
    let sql = "SELECT * FROM users WHERE id = 42 AND status = 'active'";
    c.bench_function("returning_select_skipped", |b| {
        b.iter(|| has_returning(black_box(sql)))
    });
}

fn bench_returning_insert_no_returning(c: &mut Criterion) {
    let sql = "INSERT INTO orders (customer_id, total) VALUES (42, 19.99)";
    c.bench_function("returning_insert_no_returning", |b| {
        b.iter(|| has_returning(black_box(sql)))
    });
}

fn bench_returning_insert_with_returning(c: &mut Criterion) {
    let sql = "INSERT INTO orders (customer_id, total) VALUES (42, 19.99) RETURNING id";
    c.bench_function("returning_insert_with_returning", |b| {
        b.iter(|| has_returning(black_box(sql)))
    });
}

fn bench_returning_cte_wrapped(c: &mut Criterion) {
    let sql = "WITH new_order AS (INSERT INTO orders (customer_id) VALUES (42) RETURNING id) SELECT * FROM new_order";
    c.bench_function("returning_cte_wrapped", |b| {
        b.iter(|| has_returning(black_box(sql)))
    });
}

criterion_group!(
    benches,
    bench_returning_select_skipped,
    bench_returning_insert_no_returning,
    bench_returning_insert_with_returning,
    bench_returning_cte_wrapped,
);
criterion_main!(benches);
