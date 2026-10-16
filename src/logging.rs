//! Observability foundation: rolling file + stderr logging, and a panic hook
//! that records panics through the same pipeline before delegating to the
//! default hook.

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

// The dependency default reserves 128,000 queue slots. A desktop shell needs
// a small burst buffer, not a server-sized backlog. Saturation must never
// apply disk backpressure to the message thread.
const LOG_QUEUE_LINES: usize = 512;

/// Initializes the global tracing subscriber.
///
/// Logs go to both stderr and a daily-rolling file under the log directory.
/// The returned [`WorkerGuard`] must be kept alive for the process lifetime;
/// dropping it (normally at the end of `main`) flushes buffered log lines.
pub fn init() -> WorkerGuard {
    let dir = log_dir();
    let file_appender = tracing_appender::rolling::daily(&dir, "minicon-winmgr.log");
    let (file_writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .buffered_lines_limit(LOG_QUEUE_LINES)
        .lossy(true)
        .finish(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let file_layer = fmt::layer()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_target(true);

    let stderr_layer = fmt::layer().with_writer(std::io::stderr).with_ansi(true);

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer);

    tracing::subscriber::set_global_default(subscriber)
        .expect("failed to install global tracing subscriber");

    install_panic_hook();
    tracing::info!(log_dir = %dir.display(), "observability initialized");

    guard
}

/// Resolves the log directory: `MINICON_LOG_DIR` if set, else `<exe_dir>/logs`,
/// else `./logs`. The directory is created if missing (failure is non-fatal).
fn log_dir() -> PathBuf {
    let dir = std::env::var_os("MINICON_LOG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|parent| parent.join("logs")))
        })
        .unwrap_or_else(|| PathBuf::from("logs"));

    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "minicon-winmgr: cannot create log dir {}: {err}",
            dir.display()
        );
    }
    dir
}

/// Records panics (location + payload) via tracing, then runs the previous
/// hook so default behavior (unwind / abort) is preserved. Also mirrors to
/// stderr because the non-blocking writer may not flush before abort.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| l.to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = payload_str(info.payload());

        tracing::error!(location = %location, payload = %payload, "panic");
        eprintln!("minicon-winmgr panic at {location}: {payload}");

        previous(info);
    }));
}

fn payload_str(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "isolated resource experiment; run each capacity in a fresh process"]
    fn logging_queue_commit_footprint() {
        let capacity = match std::env::var("WINMGR_TEST_LOG_QUEUE").as_deref() {
            Ok("128000") => tracing_appender::non_blocking::DEFAULT_BUFFERED_LINES_LIMIT,
            _ => LOG_QUEUE_LINES,
        };
        let before = crate::metrics::sample().private_bytes;
        let (_writer, _guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
            .buffered_lines_limit(capacity)
            .lossy(true)
            .finish(std::io::sink());
        let after = crate::metrics::sample().private_bytes;
        println!(
            "QUEUE_SAMPLE {{\"capacity\":{capacity},\"private_before\":{before},\"private_after\":{after}}}"
        );
    }

    #[test]
    fn saturated_log_queue_drops_instead_of_blocking_the_caller() {
        use std::io::Write;
        use std::time::{Duration, Instant};
        struct Slow(Option<std::sync::mpsc::Receiver<()>>);
        impl Write for Slow {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(release) = self.0.take() {
                    release.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let (mut writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
            .buffered_lines_limit(2)
            .lossy(true)
            .finish(Slow(Some(blocked)));
        let errors = writer.error_counter();
        let started = Instant::now();
        for _ in 0..1000 {
            writer.write_all(b"synthetic resource test\n").unwrap();
        }
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(errors.dropped_lines() > 0);
        release.send(()).unwrap();
        drop(guard);
    }
}
