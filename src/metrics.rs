//! Stability observability: periodic process / runtime metrics.

use std::sync::OnceLock;
use std::time::Instant;

use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{
    GR_GDIOBJECTS, GR_USEROBJECTS, GetCurrentProcess, GetGuiResources, GetProcessHandleCount,
};

/// Monotonic start point, captured on the first `sample()` call.
static START: OnceLock<Instant> = OnceLock::new();

/// One measurement of process / runtime health.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Metrics {
    pub uptime_secs: u64,
    pub working_set_bytes: u64,
    pub private_bytes: u64,
    pub handle_count: u32,
    pub gdi_objects: u32,
    pub user_objects: u32,
}

/// Diagnostic observations preserve failures as missing, rather than zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Observation {
    pub values: [Option<u64>; 5],
}
pub(crate) fn observe() -> Observation {
    use windows::Win32::Foundation::{GetLastError, SetLastError, WIN32_ERROR};
    use windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX;
    let process = unsafe { GetCurrentProcess() };
    let mut memory = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let memory_ok = unsafe {
        GetProcessMemoryInfo(
            process,
            (&mut memory as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            memory.cb,
        )
    }
    .is_ok();
    let mut handles = 0;
    let handles_ok = unsafe { GetProcessHandleCount(process, &mut handles) }.is_ok();
    let gui = |kind| unsafe {
        SetLastError(WIN32_ERROR(0));
        let count = GetGuiResources(process, kind);
        (count != 0 || GetLastError().0 == 0).then_some(count as u64)
    };
    Observation {
        values: [
            memory_ok.then_some(memory.PrivateUsage as u64),
            memory_ok.then_some(memory.WorkingSetSize as u64),
            handles_ok.then_some(handles as u64),
            gui(GR_GDIOBJECTS),
            gui(GR_USEROBJECTS),
        ],
    }
}

/// Samples current process / runtime metrics.
///
/// Best-effort: any failing Win32 call yields 0 for that field. Never panics.
pub fn sample() -> Metrics {
    let start = START.get_or_init(Instant::now);
    let uptime_secs = start.elapsed().as_secs();

    let process = unsafe { GetCurrentProcess() };

    let mut mem = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    let (working_set_bytes, private_bytes) =
        match unsafe { GetProcessMemoryInfo(process, &mut mem, mem.cb) } {
            Ok(()) => (mem.WorkingSetSize as u64, mem.PagefileUsage as u64),
            Err(_) => (0, 0),
        };

    let mut handles: u32 = 0;
    let handle_count = if unsafe { GetProcessHandleCount(process, &mut handles) }.is_ok() {
        handles
    } else {
        0
    };

    let gdi_objects = unsafe { GetGuiResources(process, GR_GDIOBJECTS) };
    let user_objects = unsafe { GetGuiResources(process, GR_USEROBJECTS) };

    Metrics {
        uptime_secs,
        working_set_bytes,
        private_bytes,
        handle_count,
        gdi_objects,
        user_objects,
    }
}

/// Logs a metrics line (info level) with a stable field set.
pub fn log_sample() {
    let m = sample();
    tracing::info!(
        line = %format_line(&m),
        uptime_secs = m.uptime_secs,
        working_set_bytes = m.working_set_bytes,
        private_bytes = m.private_bytes,
        handle_count = m.handle_count,
        gdi_objects = m.gdi_objects,
        user_objects = m.user_objects,
        "metrics"
    );
}

/// Formats metrics as a single stable line (pure; used by tests).
///
/// Field order is fixed: `uptime=..s ws=..MB priv=..MB handles=.. gdi=.. user=..`
/// Memory figures are rendered in MB with one decimal place.
pub fn format_line(m: &Metrics) -> String {
    format!(
        "uptime={}s ws={:.1}MB priv={:.1}MB handles={} gdi={} user={}",
        m.uptime_secs,
        m.working_set_bytes as f64 / (1024.0 * 1024.0),
        m.private_bytes as f64 / (1024.0 * 1024.0),
        m.handle_count,
        m.gdi_objects,
        m.user_objects,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_line_has_stable_fields() {
        let m = Metrics {
            uptime_secs: 10,
            working_set_bytes: 45 * 1024 * 1024,
            private_bytes: 30 * 1024 * 1024,
            handle_count: 8,
            gdi_objects: 1,
            user_objects: 2,
        };
        let line = format_line(&m);
        assert!(line.contains("uptime=10s"), "line was: {line}");
        assert!(line.contains("handles=8"), "line was: {line}");
        assert!(line.contains("gdi=1"), "line was: {line}");
        assert!(line.contains("user=2"), "line was: {line}");
        // Memory rendered in MB with one decimal.
        assert!(line.contains("ws=45.0MB"), "line was: {line}");
        assert!(line.contains("priv=30.0MB"), "line was: {line}");
        // Single line, no embedded newlines.
        assert!(!line.contains('\n'), "line was: {line}");
    }

    #[test]
    fn sample_does_not_panic() {
        let m = sample();
        // Uptime is monotonic and non-negative; simply exercise the path.
        let _ = m.uptime_secs;
        let _ = format_line(&m);
    }
}
