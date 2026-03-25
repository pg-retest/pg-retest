use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dashmap::DashMap;
use pg_retest::correlate::substitute::substitute_ids;

fn bench_substitute_no_map(c: &mut Criterion) {
    let map = DashMap::new();
    let sql = "SELECT * FROM orders WHERE id = 42 AND customer_id = 99 AND status = 'active'";
    c.bench_function("substitute_no_map", |b| {
        b.iter(|| substitute_ids(black_box(sql), &map))
    });
}

fn bench_substitute_small_map(c: &mut Criterion) {
    let map = DashMap::new();
    for i in 0..10 {
        map.insert(format!("{}", 40 + i), format!("{}", 1000 + i));
    }
    let sql = "SELECT * FROM orders WHERE id = 42 AND customer_id = 43 AND status = 'active'";
    c.bench_function("substitute_small_map", |b| {
        b.iter(|| substitute_ids(black_box(sql), &map))
    });
}

fn bench_substitute_large_map(c: &mut Criterion) {
    let map = DashMap::new();
    for i in 0..10_000 {
        map.insert(format!("{}", i), format!("{}", i + 100_000));
    }
    let sql = "SELECT * FROM orders WHERE id = 42 AND customer_id = 9999 AND status = 'active'";
    c.bench_function("substitute_large_map", |b| {
        b.iter(|| substitute_ids(black_box(sql), &map))
    });
}

fn bench_substitute_complex_query(c: &mut Criterion) {
    let map = DashMap::new();
    for i in 0..100 {
        map.insert(format!("{}", i), format!("{}", i + 100_000));
    }
    let sql = "WITH cte AS (SELECT id, name FROM customers WHERE region_id = 42 AND tier = 3) \
               SELECT o.id, o.total, c.name FROM orders o \
               JOIN cte c ON c.id = o.customer_id \
               WHERE o.status_id = 7 AND o.amount > 50 \
               ORDER BY o.created_at DESC LIMIT 100 OFFSET 20";
    c.bench_function("substitute_complex_query", |b| {
        b.iter(|| substitute_ids(black_box(sql), &map))
    });
}

fn bench_substitute_uuid_heavy(c: &mut Criterion) {
    let map = DashMap::new();
    for i in 0..1000 {
        map.insert(
            format!("{:08x}-0000-0000-0000-{:012x}", i, i),
            format!("{:08x}-ffff-ffff-ffff-{:012x}", i, i),
        );
    }
    let sql = "SELECT * FROM t WHERE \
               a = '00000005-0000-0000-0000-000000000005' AND \
               b = '00000010-0000-0000-0000-000000000010' AND \
               c = '00000050-0000-0000-0000-000000000050' AND \
               d = '00000100-0000-0000-0000-000000000100' AND \
               e = '00000500-0000-0000-0000-000000000500'";
    c.bench_function("substitute_uuid_heavy", |b| {
        b.iter(|| substitute_ids(black_box(sql), &map))
    });
}

fn bench_returning_detection(c: &mut Criterion) {
    use pg_retest::correlate::capture::has_returning;
    let queries: Vec<String> = (0..1000)
        .map(|i| {
            if i % 3 == 0 {
                format!("INSERT INTO t{} (x) VALUES ({}) RETURNING id", i, i)
            } else if i % 3 == 1 {
                format!("SELECT * FROM t{} WHERE id = {}", i, i)
            } else {
                format!("UPDATE t{} SET x = {} WHERE id = {}", i, i, i)
            }
        })
        .collect();
    c.bench_function("returning_detection_1000", |b| {
        b.iter(|| {
            for q in &queries {
                black_box(has_returning(q));
            }
        })
    });
}

criterion_group!(
    benches,
    bench_substitute_no_map,
    bench_substitute_small_map,
    bench_substitute_large_map,
    bench_substitute_complex_query,
    bench_substitute_uuid_heavy,
    bench_returning_detection,
);
criterion_main!(benches);
