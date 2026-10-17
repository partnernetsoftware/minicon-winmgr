//! Observability foundation: rolling file + stderr logging, and a panic hook
//! that withholds private text in fields, spans and panic output.

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

/// Text fields can contain paths, commands, window titles or error chains.
/// Keep event messages (audited static literals) and primitive counters only.
/// This applies at every verbosity, to both sinks and to span fields.
#[derive(Clone, Copy)]
struct PrivateFields;
struct PrivateVisitor<'a> {
    writer: fmt::format::Writer<'a>,
    result: std::fmt::Result,
}
impl PrivateVisitor<'_> {
    fn value(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if self.result.is_ok() {
            self.result = if field.name() == "message" {
                write!(self.writer, "{value:?}")
            } else {
                write!(self.writer, " {}={value:?}", field.name())
            };
        }
    }
}
impl tracing::field::Visit for PrivateVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.value(field, value);
        } else {
            self.value(field, &"[redacted]");
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, _: &str) {
        self.value(field, &"[redacted]");
    }
    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        _: &(dyn std::error::Error + 'static),
    ) {
        self.value(field, &"[redacted]");
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.value(field, &value);
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.value(field, &value);
    }
    fn record_u128(&mut self, field: &tracing::field::Field, value: u128) {
        self.value(field, &value);
    }
    fn record_i128(&mut self, field: &tracing::field::Field, value: i128) {
        self.value(field, &value);
    }
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.value(field, &value);
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.value(field, &value);
    }
}
impl<'writer> fmt::FormatFields<'writer> for PrivateFields {
    fn format_fields<R: tracing_subscriber::field::RecordFields>(
        &self,
        writer: fmt::format::Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        let mut visitor = PrivateVisitor {
            writer,
            result: Ok(()),
        };
        fields.record(&mut visitor);
        visitor.result
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
        .fmt_fields(PrivateFields)
        .event_format(BoundedFormat(
            fmt::format().with_ansi(false).with_target(true),
        ));

    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .fmt_fields(PrivateFields)
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

/// Resolves the log directory. `MINICON_LOG_DIR` wins verbatim — support
/// bundles and tests pin the exact directory.
///
/// The built-in path is segmented per Windows account. One executable can be
/// started by more than one account: every interactive session runs its own
/// shell instance, and all of them resolve to the same install directory.
/// Sharing one directory interleaves their log lines — and even serialises
/// them through one lock file — which makes a single user's failure report
/// unreadable exactly when it matters.
fn resolve_log_dir(explicit: Option<std::ffi::OsString>) -> PathBuf {
    if let Some(explicit) = explicit {
        return PathBuf::from(explicit);
    }
    let base = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join("logs")))
        .unwrap_or_else(|| PathBuf::from("logs"));
    base.join(user_segment())
}

fn log_dir() -> PathBuf {
    resolve_log_dir(std::env::var_os("MINICON_LOG_DIR"))
}

/// One path component naming the account this process runs as.
fn user_segment() -> String {
    let identity = crate::privilege::account_identity();
    sanitize_component(&identity).unwrap_or_else(|| "user".to_string())
}

/// Longest account name kept verbatim; anything longer is digested.
const SEGMENT_CHARS: usize = 48;

/// Reduces an account identity to a single safe path component.
///
/// Account names may contain spaces, non-ASCII characters or separators. Every
/// disallowed character becomes `_`, so the segment can never escape the log
/// directory or collide with a reserved name. When the reduction is lossy a
/// short digest of the original is appended: two names that collapse onto the
/// same characters (`张 三` and `李 四`) must still get different directories,
/// or the very sharing this fixes would come back through the back door.
fn sanitize_component(raw: &str) -> Option<String> {
    let mut mapped: String = raw
        .chars()
        .take(SEGMENT_CHARS)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let lossy = mapped.chars().ne(raw.chars());
    if lossy {
        mapped.push('-');
        mapped.push_str(&format!("{:08x}", digest(raw)));
    }
    // Windows path components must not end in `.` and `.`/`..` are reserved.
    let trimmed = mapped.trim_matches('.').to_string();
    (!trimmed.is_empty() && trimmed != "." && trimmed != "..").then_some(trimmed)
}

/// FNV-1a over the UTF-16 code units: a short, stable discriminator appended
/// when sanitisation changed the name.
fn digest(raw: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for unit in raw.encode_utf16() {
        for byte in unit.to_le_bytes() {
            hash ^= u32::from(byte);
            hash = hash.wrapping_mul(0x0100_0193);
        }
    }
    hash
}

/// Panic payloads and source paths may contain private data. Replacing the
/// output hook does not change Rust's unwind/abort behavior. Never delegate to
/// the default hook, which prints the payload and may print an absolute path.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let line = info.location().map_or(0, |location| location.line());
        let column = info.location().map_or(0, |location| location.column());
        tracing::error!(
            line,
            column,
            "panic; private payload and source path withheld"
        );
        // The nonblocking queue may not flush before abort.
        eprintln!("minicon-winmgr panic at source line {line}:{column}; private details withheld");
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_log_directory_is_used_verbatim() {
        let pinned = std::ffi::OsString::from("D:/support-bundle/logs");
        assert_eq!(
            resolve_log_dir(Some(pinned.clone())),
            PathBuf::from(&pinned)
        );
    }

    #[test]
    fn builtin_log_directory_is_segmented_per_account() {
        let dir = resolve_log_dir(None);
        let segment = dir
            .file_name()
            .expect("segment")
            .to_string_lossy()
            .into_owned();
        assert_eq!(segment, user_segment());
        assert!(!segment.is_empty());
        assert!(!segment.contains(['\\', '/', ':']));
        assert!(!matches!(segment.as_str(), "." | ".."));
        assert_eq!(
            dir.parent()
                .and_then(|parent| parent.file_name())
                .map(|name| name.to_string_lossy().into_owned()),
            Some("logs".to_string())
        );
    }

    #[test]
    fn plain_account_names_pass_through_unchanged() {
        for raw in ["wjc2022", "user.name", "a_b-c", "User1"] {
            assert_eq!(sanitize_component(raw).as_deref(), Some(raw));
        }
    }

    #[test]
    fn unsafe_account_names_are_reduced_and_disambiguated() {
        let spaced = sanitize_component("张 三").expect("segment");
        let other = sanitize_component("李 四").expect("segment");
        for segment in [&spaced, &other] {
            assert!(
                segment
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
            );
        }
        // Same reduced characters, different account: the digest must keep the
        // two directories apart.
        assert_eq!(spaced.split('-').next(), other.split('-').next());
        assert_ne!(spaced, other);
        let separator = sanitize_component("..\\..\\evil").expect("segment");
        assert!(!separator.contains(['\\', '/']));
        assert_ne!(separator, sanitize_component("evil").expect("segment"));
    }

    #[test]
    fn long_account_names_are_truncated_but_stay_unique() {
        let long = "a".repeat(80);
        let short = sanitize_component(&long).expect("segment");
        assert!(short.len() <= 48 + 1 + 8);
        assert_ne!(
            short,
            sanitize_component(&format!("{long}b")).expect("segment")
        );
    }

    #[test]
    fn reserved_and_empty_identities_have_no_segment() {
        for raw in ["", ".", "..", "..."] {
            assert_eq!(sanitize_component(raw), None);
        }
        assert_eq!(
            user_segment_for_test(""),
            "user".to_string(),
            "an unusable identity must fall back, never to an empty component"
        );
    }

    /// `user_segment` reads the live token; this is the same fallback rule with
    /// an identity that no token can produce.
    fn user_segment_for_test(identity: &str) -> String {
        sanitize_component(identity).unwrap_or_else(|| "user".to_string())
    }

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
    fn event_messages_are_static_literals_without_interpolation() {
        use syn::visit::Visit;
        struct Audit;
        impl<'ast> Visit<'ast> for Audit {
            fn visit_macro(&mut self, node: &'ast syn::Macro) {
                let name = node.path.segments.last().unwrap().ident.to_string();
                if name == "span" || name.ends_with("_span") {
                    let tokens: Vec<_> = node.tokens.clone().into_iter().collect();
                    assert!(
                        !tokens
                            .windows(2)
                            .any(|pair| pair[0].to_string() == "message"
                                && pair[1].to_string() == "="),
                        "message is reserved for static event text"
                    );
                }
                if ["trace", "debug", "info", "warn", "error", "event"].contains(&name.as_str()) {
                    let tokens: Vec<_> = node.tokens.clone().into_iter().collect();
                    assert!(
                        !tokens
                            .windows(2)
                            .any(|pair| pair[0].to_string() == "message"
                                && pair[1].to_string() == "="),
                        "message is reserved for static event text"
                    );
                    let last = tokens.iter().rev().find(|t| t.to_string() != ",").unwrap();
                    let text = syn::parse_str::<syn::LitStr>(&last.to_string()).expect(
                        "log messages must end with a static string literal; put values in fields",
                    );
                    assert!(
                        !text.value().contains(['{', '}']),
                        "log messages must not interpolate private values"
                    );
                }
                syn::visit::visit_macro(self, node);
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|s| s == "rs") {
                let source = std::fs::read_to_string(&path).unwrap();
                Audit.visit_file(&syn::parse_file(&source).unwrap());
            }
        }
    }

    #[test]
    #[ignore = "private child fixture; invoked by privacy test with an isolated log directory"]
    fn private_log_fixture() {
        assert!(std::env::var_os("WINMGR_TEST_PRIVATE_LOG").is_some());
        let guard = init();
        let secret = "LOG_PRIVATE_SENTINEL";
        struct NeverFormat;
        impl std::fmt::Debug for NeverFormat {
            fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                panic!("redacted Debug must not run");
            }
        }
        impl std::fmt::Display for NeverFormat {
            fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                panic!("redacted Display must not run");
            }
        }
        let span = tracing::info_span!("synthetic_operation", path = secret, input = %secret);
        let _entered = span.enter();
        tracing::info!(path = secret, title = ?NeverFormat, error = %NeverFormat, count = 17u64, ready = true, "synthetic operation succeeded");
        let error = std::io::Error::other(secret);
        tracing::error!(
            error = &error as &(dyn std::error::Error + 'static),
            "synthetic operation failed"
        );
        tracing::trace!(input = %secret, query = ?secret, "synthetic verbose event");
        assert!(std::panic::catch_unwind(|| panic!("{secret}")).is_err());
        drop(guard);
    }

    #[test]
    fn file_stderr_spans_and_panic_do_not_emit_private_text_even_at_trace_level() {
        let directory = std::env::temp_dir()
            .join("minicon-winmgr-test-reports/log-privacy")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "logging::tests::private_log_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("WINMGR_TEST_PRIVATE_LOG", "1")
            .env("MINICON_LOG_DIR", &directory)
            .env("RUST_LOG", "trace")
            .env("RUST_BACKTRACE", "full")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "private child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        let file = std::fs::read_to_string(directory.join("minicon-winmgr.current.log")).unwrap();
        for text in [&stderr, &file] {
            assert!(text.contains("synthetic operation succeeded"));
            assert!(text.contains("synthetic operation failed"));
            assert!(text.contains("synthetic verbose event"));
            assert!(text.contains("count=17"));
            assert!(text.contains("ready=true"));
            assert!(text.contains("panic; private payload"));
            assert!(!text.contains("LOG_PRIVATE_SENTINEL"));
            assert!(!text.contains(env!("CARGO_MANIFEST_DIR")));
            assert!(!text.contains("redacted Debug must not run"));
            assert!(!text.contains("redacted Display must not run"));
        }
        assert!(
            !String::from_utf8(output.stdout)
                .unwrap()
                .contains("LOG_PRIVATE_SENTINEL")
        );
        std::fs::remove_dir_all(directory).unwrap();
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
