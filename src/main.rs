use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use pg_retest::cli::{Cli, Commands};

fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    match cli.command {
        Commands::Capture(args) => cmd_capture(args),
        Commands::Replay(args) => cmd_replay(args),
        Commands::Compare(args) => cmd_compare(args),
        Commands::Inspect(args) => cmd_inspect(args),
    }
}

fn cmd_capture(args: pg_retest::cli::CaptureArgs) -> Result<()> {
    use pg_retest::capture::csv_log::CsvLogCapture;
    use pg_retest::profile::io;

    let capture = CsvLogCapture;
    let profile = capture.capture_from_file(&args.source_log, &args.source_host, &args.pg_version)?;

    println!(
        "Captured {} queries across {} sessions",
        profile.metadata.total_queries, profile.metadata.total_sessions
    );

    io::write_profile(&args.output, &profile)?;
    println!("Wrote workload profile to {}", args.output.display());
    Ok(())
}

fn cmd_replay(_args: pg_retest::cli::ReplayArgs) -> Result<()> {
    anyhow::bail!("Replay not yet implemented")
}

fn cmd_compare(_args: pg_retest::cli::CompareArgs) -> Result<()> {
    anyhow::bail!("Compare not yet implemented")
}

fn cmd_inspect(args: pg_retest::cli::InspectArgs) -> Result<()> {
    use pg_retest::profile::io;

    let profile = io::read_profile(&args.path)?;
    let json = serde_json::to_string_pretty(&profile)?;
    println!("{json}");
    Ok(())
}
