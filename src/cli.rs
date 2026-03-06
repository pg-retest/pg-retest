use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "pg-retest")]
#[command(version, about = "Capture, replay, and compare PostgreSQL workloads")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Capture workload from PostgreSQL logs
    Capture(CaptureArgs),

    /// Replay a captured workload against a target database
    Replay(ReplayArgs),

    /// Compare source workload with replay results
    Compare(CompareArgs),

    /// Inspect a workload profile file
    Inspect(InspectArgs),

    /// Run a capture proxy between clients and PostgreSQL
    Proxy(ProxyArgs),

    /// Run full CI/CD pipeline (capture → provision → replay → compare)
    Run(RunArgs),

    /// Compare replay performance across different database targets
    #[command(name = "ab")]
    AB(ABArgs),
}

#[derive(clap::Args)]
pub struct CaptureArgs {
    /// Path to source log file (required for pg-csv, mysql-slow)
    #[arg(long)]
    pub source_log: Option<PathBuf>,

    /// Source log type: pg-csv (default), mysql-slow, rds
    #[arg(long, default_value = "pg-csv")]
    pub source_type: String,

    /// Output workload profile path (.wkl)
    #[arg(short, long, default_value = "workload.wkl")]
    pub output: PathBuf,

    /// Source host identifier (for metadata)
    #[arg(long, default_value = "unknown")]
    pub source_host: String,

    /// PostgreSQL version (for metadata)
    #[arg(long, default_value = "unknown")]
    pub pg_version: String,

    /// Mask string and numeric literals in captured SQL (PII protection)
    #[arg(long, default_value_t = false)]
    pub mask_values: bool,

    /// RDS instance identifier (for --source-type rds)
    #[arg(long)]
    pub rds_instance: Option<String>,

    /// AWS region for RDS instance
    #[arg(long, default_value = "us-east-1")]
    pub rds_region: String,

    /// Specific RDS log file to download (omit to use latest)
    #[arg(long)]
    pub rds_log_file: Option<String>,
}

#[derive(clap::Args)]
pub struct ReplayArgs {
    /// Path to workload profile (.wkl)
    #[arg(long)]
    pub workload: PathBuf,

    /// Target PostgreSQL connection string
    #[arg(long)]
    pub target: String,

    /// Output results profile path (.wkl)
    #[arg(short, long, default_value = "results.wkl")]
    pub output: PathBuf,

    /// Replay only SELECT queries (strip DML)
    #[arg(long, default_value_t = false)]
    pub read_only: bool,

    /// Speed multiplier (e.g., 2.0 = 2x faster)
    #[arg(long, default_value_t = 1.0)]
    pub speed: f64,

    /// Scale factor: duplicate sessions N times for load testing
    #[arg(long, default_value_t = 1)]
    pub scale: u32,

    /// Stagger interval between scaled copies (milliseconds)
    #[arg(long, default_value_t = 0)]
    pub stagger_ms: u64,

    /// Scale analytical sessions by N (per-category scaling)
    #[arg(long)]
    pub scale_analytical: Option<u32>,

    /// Scale transactional sessions by N (per-category scaling)
    #[arg(long)]
    pub scale_transactional: Option<u32>,

    /// Scale mixed sessions by N (per-category scaling)
    #[arg(long)]
    pub scale_mixed: Option<u32>,

    /// Scale bulk sessions by N (per-category scaling)
    #[arg(long)]
    pub scale_bulk: Option<u32>,
}

#[derive(clap::Args)]
pub struct CompareArgs {
    /// Source workload profile (.wkl)
    #[arg(long)]
    pub source: PathBuf,

    /// Replay results profile (.wkl)
    #[arg(long)]
    pub replay: PathBuf,

    /// Output JSON report path
    #[arg(long)]
    pub json: Option<PathBuf>,

    /// Regression threshold percentage (flag queries slower by this %)
    #[arg(long, default_value_t = 20.0)]
    pub threshold: f64,

    /// Exit non-zero if regressions are detected
    #[arg(long, default_value_t = false)]
    pub fail_on_regression: bool,

    /// Exit non-zero if query errors occurred
    #[arg(long, default_value_t = false)]
    pub fail_on_error: bool,
}

#[derive(clap::Args)]
pub struct InspectArgs {
    /// Path to workload profile (.wkl)
    pub path: PathBuf,

    /// Show workload classification breakdown
    #[arg(long, default_value_t = false)]
    pub classify: bool,
}

#[derive(clap::Args)]
pub struct ProxyArgs {
    /// Address to listen on (e.g., 0.0.0.0:5433)
    #[arg(long, default_value = "0.0.0.0:5433")]
    pub listen: String,

    /// Target PostgreSQL address (e.g., localhost:5432)
    #[arg(long)]
    pub target: String,

    /// Output workload profile path (.wkl)
    #[arg(short, long, default_value = "workload.wkl")]
    pub output: PathBuf,

    /// Maximum server connections in the pool
    #[arg(long, default_value_t = 100)]
    pub pool_size: usize,

    /// Timeout waiting for a pool connection (seconds)
    #[arg(long, default_value_t = 30)]
    pub pool_timeout: u64,

    /// Mask string and numeric literals in captured SQL (PII protection)
    #[arg(long, default_value_t = false)]
    pub mask_values: bool,

    /// Disable workload capture (proxy-only mode)
    #[arg(long, default_value_t = false)]
    pub no_capture: bool,

    /// Capture duration (e.g., 60s, 5m). If not set, runs until Ctrl+C.
    #[arg(long)]
    pub duration: Option<String>,
}

#[derive(clap::Args)]
pub struct RunArgs {
    /// Path to pipeline config file (.toml)
    #[arg(long, default_value = ".pg-retest.toml")]
    pub config: PathBuf,
}

#[derive(clap::Args)]
pub struct ABArgs {
    /// Path to workload profile (.wkl)
    #[arg(long)]
    pub workload: PathBuf,

    /// Variant definitions: "label=connection_string" (specify 2+ times)
    #[arg(long = "variant", required = true, num_args = 2..)]
    pub variants: Vec<String>,

    /// Replay only SELECT queries
    #[arg(long, default_value_t = false)]
    pub read_only: bool,

    /// Speed multiplier
    #[arg(long, default_value_t = 1.0)]
    pub speed: f64,

    /// Output JSON report path
    #[arg(long)]
    pub json: Option<PathBuf>,

    /// Regression threshold percentage
    #[arg(long, default_value_t = 20.0)]
    pub threshold: f64,
}
