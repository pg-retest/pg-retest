use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use super::WorkloadProfile;

pub fn write_profile(path: &Path, profile: &WorkloadProfile) -> Result<()> {
    let bytes = rmp_serde::to_vec(profile)
        .context("Failed to serialize workload profile to MessagePack")?;
    fs::write(path, bytes)
        .with_context(|| format!("Failed to write profile to {}", path.display()))?;
    Ok(())
}

pub fn read_profile(path: &Path) -> Result<WorkloadProfile> {
    let bytes = fs::read(path)
        .with_context(|| format!("Failed to read profile from {}", path.display()))?;
    let profile: WorkloadProfile = rmp_serde::from_slice(&bytes)
        .context("Failed to deserialize workload profile from MessagePack")?;
    Ok(profile)
}
