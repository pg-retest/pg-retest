use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn substitute_bench(_c: &mut Criterion) {
    // Placeholder benchmark
}

criterion_group!(benches, substitute_bench);
criterion_main!(benches);
