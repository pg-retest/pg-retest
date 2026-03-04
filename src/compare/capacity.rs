use serde::{Deserialize, Serialize};

use crate::replay::ReplayResults;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaleReport {
    pub scale_factor: u32,
    pub total_sessions: u64,
    pub total_queries: u64,
    pub throughput_qps: f64,
    pub avg_latency_us: u64,
    pub p95_latency_us: u64,
    pub p99_latency_us: u64,
    pub error_count: u64,
    pub error_rate_pct: f64,
}

pub fn compute_scale_report(
    results: &[ReplayResults],
    scale_factor: u32,
    elapsed_us: u64,
) -> ScaleReport {
    let total_sessions = results.len() as u64;
    let mut all_latencies: Vec<u64> = Vec::new();
    let mut error_count: u64 = 0;

    for r in results {
        for qr in &r.query_results {
            all_latencies.push(qr.replay_duration_us);
            if !qr.success {
                error_count += 1;
            }
        }
    }

    let total_queries = all_latencies.len() as u64;
    all_latencies.sort();

    let avg_latency_us = if total_queries > 0 {
        (all_latencies.iter().sum::<u64>() as f64 / total_queries as f64).round() as u64
    } else {
        0
    };

    let throughput_qps = if elapsed_us > 0 {
        total_queries as f64 / (elapsed_us as f64 / 1_000_000.0)
    } else {
        0.0
    };

    let error_rate_pct = if total_queries > 0 {
        error_count as f64 / total_queries as f64 * 100.0
    } else {
        0.0
    };

    ScaleReport {
        scale_factor,
        total_sessions,
        total_queries,
        throughput_qps,
        avg_latency_us,
        p95_latency_us: percentile(&all_latencies, 95),
        p99_latency_us: percentile(&all_latencies, 99),
        error_count,
        error_rate_pct,
    }
}

pub fn print_scale_report(report: &ScaleReport) {
    println!();
    println!("  Scaled Replay Report");
    println!("  ====================");
    println!();
    println!("  Scale factor:    {}x", report.scale_factor);
    println!("  Total sessions:  {}", report.total_sessions);
    println!("  Total queries:   {}", report.total_queries);
    println!(
        "  Throughput:      {:.1} queries/sec",
        report.throughput_qps
    );
    println!(
        "  Avg latency:     {:.2} ms",
        report.avg_latency_us as f64 / 1000.0
    );
    println!(
        "  P95 latency:     {:.2} ms",
        report.p95_latency_us as f64 / 1000.0
    );
    println!(
        "  P99 latency:     {:.2} ms",
        report.p99_latency_us as f64 / 1000.0
    );
    println!("  Errors:          {}", report.error_count);
    println!("  Error rate:      {:.2}%", report.error_rate_pct);
    println!();
}

fn percentile(sorted: &[u64], pct: u32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((pct as f64 / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}
