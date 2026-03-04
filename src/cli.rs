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
}

#[derive(clap::Args)]
pub struct CaptureArgs {
    /// Path to PostgreSQL CSV log file
    #[arg(long)]
    pub source_log: PathBuf,

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
