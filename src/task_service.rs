//! Portable task data and identity rules. Native handles stay in the adapter.
use serde::{Deserialize, Serialize};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::Collector;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Identity {
    pub pid: u32,
    pub created_100ns: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Process {
    pub identity: Identity,
    pub parent_pid: u32,
    pub name: String,
    pub owner: String,
    pub session: Option<u32>,
    pub path: Option<String>,
    pub cpu_percent: Option<f64>,
    pub working_bytes: Option<u64>,
    pub private_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Endpoint {
    pub process: Identity,
    pub protocol: String,
    pub local: String,
    pub remote: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: u32,
    pub platform: String,
    pub sampled_unix_ms: u128,
    pub owner: String,
    pub session: u32,
    pub processes: Vec<Process>,
    pub endpoints: Vec<Endpoint>,
    pub ownership_unavailable: usize,
    pub limitations: Vec<String>,
}

/// Whole-machine percentage; a recycled PID must never inherit an old sample.
fn cpu_percent(
    previous: Option<(Identity, u64)>,
    identity: Identity,
    total_100ns: u64,
    elapsed_seconds: f64,
    logical_processors: usize,
) -> Option<f64> {
    let (old, prior) = previous?;
    if old != identity || elapsed_seconds <= 0.0 || !elapsed_seconds.is_finite() {
        return None;
    }
    let delta = total_100ns.checked_sub(prior)?;
    Some(
        (delta as f64 / 100_000.0 / elapsed_seconds / logical_processors.max(1) as f64)
            .clamp(0.0, 100.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recycled_pid_and_regressing_counters_do_not_inherit_usage() {
        let old = Identity {
            pid: 42,
            created_100ns: 100,
        };
        let new = Identity {
            pid: 42,
            created_100ns: 200,
        };
        assert_eq!(
            cpu_percent(Some((old, 10_000_000)), new, 15_000_000, 1.0, 2),
            None
        );
        assert_eq!(cpu_percent(Some((old, 10_000_000)), old, 5, 1.0, 2), None);
        assert_eq!(
            cpu_percent(Some((old, 10_000_000)), old, 15_000_000, 1.0, 2),
            Some(25.0)
        );
    }
}
