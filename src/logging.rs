//! Observability foundation: rolling file + stderr logging, and a panic hook
//! that records panics through the same pipeline before delegating to the
//! default hook.

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const TRUNCATED: &str = " [truncated]\n";
struct BoundedText {
    text: String,
    truncated: bool,
}
impl BoundedText {
    fn new() -> Self {
        Self {
            text: String::new(),
            truncated: false,
        }
    }
}
impl std::fmt::Write for BoundedText {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        let room =
            (crate::log_store::RECORD_BYTES - TRUNCATED.len()).saturating_sub(self.text.len());
        let mut end = room.min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&value[..end]);
        if end < value.len() {
            self.truncated = true;
            Err(std::fmt::Error)
        } else {
            Ok(())
        }
    }
}
struct BoundedFormat<E>(E);
impl<S, N, E> fmt::FormatEvent<S, N> for BoundedFormat<E>
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    N: for<'writer> fmt::FormatFields<'writer> + 'static,
    E: fmt::FormatEvent<S, N>,
{
    fn format_event(
        &self,
        ctx: &fmt::FmtContext<'_, S, N>,
        mut writer: fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let mut text = BoundedText::new();
        let result = self
            .0
            .format_event(ctx, fmt::format::Writer::new(&mut text), event);
        if !text.truncated {
            result?;
        }
        writer.write_str(&text.text)?;
        if text.truncated {
            writer.write_str(TRUNCATED)?;
        }
        Ok(())
    }
}

// The dependency default reserves 128,000 queue slots. A desktop shell needs
// a small burst buffer, not a server-sized backlog. Saturation must never
// apply disk backpressure to the message thread.
const LOG_QUEUE_LINES: usize = 512;

/// Initializes the global tracing subscriber.
///
/// Logs go to stderr and a fixed-size file group under the log directory.
/// The returned [`WorkerGuard`] must be kept alive for the process lifetime;
/// dropping it (normally at the end of `main`) flushes buffered log lines.
pub fn init() -> WorkerGuard {
    let dir = log_dir();
    let file_appender = crate::log_store::Store::new(dir);
    let (file_writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .buffered_lines_limit(LOG_QUEUE_LINES)
        .lossy(true)
        .finish(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let file_layer = fmt::layer()
        .with_writer(file_writer)
        .event_format(BoundedFormat(
            fmt::format().with_ansi(false).with_target(true),
        ));

    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .event_format(BoundedFormat(fmt::format().with_ansi(true)));

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer);

    tracing::subscriber::set_global_default(subscriber)
        .expect("failed to install global tracing subscriber");

    install_panic_hook();
    tracing::info!("observability initialized");

    guard
}

/// Resolves the log directory: `MINICON_LOG_DIR` if set, else `<exe_dir>/logs`,
/// else `./logs`. Directory creation is deferred to the file worker.
fn log_dir() -> PathBuf {
    std::env::var_os("MINICON_LOG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|parent| parent.join("logs")))
        })
        .unwrap_or_else(|| PathBuf::from("logs"))
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
    fn formatted_events_are_utf8_bounded_and_the_next_event_still_works() {
        #[derive(Clone)]
        struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> fmt::MakeWriter<'a> for Capture {
            type Writer = Self;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }
        let capture = Capture(Default::default());
        let subscriber = tracing_subscriber::registry().with(
            fmt::layer()
                .with_writer(capture.clone())
                .event_format(BoundedFormat(fmt::format().with_ansi(false).without_time())),
        );
        tracing::subscriber::with_default(subscriber, || {
            let huge = "中文🪟".repeat(10_000);
            tracing::info!(payload = %huge, "oversized");
            let bytes = capture.0.lock().unwrap();
            assert!(bytes.len() <= crate::log_store::RECORD_BYTES);
            assert!(std::str::from_utf8(&bytes).unwrap().ends_with(TRUNCATED));
            drop(bytes);
            capture.0.lock().unwrap().clear();
            tracing::info!("next event");
            let bytes = capture.0.lock().unwrap();
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(text.contains("next event"));
            assert!(!text.contains("truncated"));
        });
    }

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
