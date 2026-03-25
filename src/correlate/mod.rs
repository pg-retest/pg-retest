pub mod capture;
pub mod map;
pub mod sequence;
pub mod substitute;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// ID handling mode for capture and replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdMode {
    /// No ID handling (default, current behavior)
    #[default]
    None,
    /// Snapshot sequences at capture, reset on target before replay
    Sequence,
    /// Capture RETURNING values via proxy, substitute during replay
    Correlate,
    /// Sequence reset + correlation combined
    Full,
}

impl IdMode {
    /// Whether this mode requires sequence snapshot/restore.
    pub fn needs_sequences(&self) -> bool {
        matches!(self, IdMode::Sequence | IdMode::Full)
    }

    /// Whether this mode requires correlation (proxy RETURNING capture).
    pub fn needs_correlation(&self) -> bool {
        matches!(self, IdMode::Correlate | IdMode::Full)
    }
}
