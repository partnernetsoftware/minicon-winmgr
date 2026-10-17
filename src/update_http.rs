//! Bounded WinHTTP requests and CNG SHA-256, used only by update workers.
use crate::win_util::wide;
use std::ffi::c_void;
use std::time::{Duration, Instant};
use windows::Win32::Networking::WinHttp::*;
use windows::core::PCWSTR;

#[cfg(test)]
pub(crate) fn sha256(bytes: &[u8]) -> Result<String, String> {
    crate::update_stream::hash_reader(bytes, bytes.len())
}

#[derive(Debug)]
pub(crate) struct Url {
    host: String,
    port: u16,
    path: String,
    secure: bool,
}
impl Url {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        if value.len() > 4096
            || value.bytes().any(|b| b <= 32 || b == 127)
            || value.contains(['@', '#', '\\'])
        {
            return Err("invalid update URL".into());
        }
        let (scheme, rest) = value.split_once("://").ok_or("missing URL scheme")?;
        let (authority, path) = rest.split_once('/').ok_or("missing URL path")?;
        let (host, port) = authority
            .split_once(':')
            .unwrap_or((authority, if scheme == "https" { "443" } else { "80" }));
        let loopback = matches!(host, "127.0.0.1" | "localhost");
        if host.is_empty() || !(scheme == "https" || scheme == "http" && loopback) {
            return Err("updates require HTTPS (except local test servers)".into());
        }
        Ok(Self {
            host: host.into(),
            port: port.parse().map_err(|_| "invalid URL port")?,
            path: format!("/{path}"),
            secure: scheme == "https",
        })
    }
}
pub(crate) fn loopback(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| matches!(url.host.as_str(), "127.0.0.1" | "localhost"))
}

struct Internet(*mut c_void);
impl Internet {
    fn new(raw: *mut c_void) -> Result<Self, String> {
        if raw.is_null() {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(Self(raw))
        }
    }
}
impl Drop for Internet {
    fn drop(&mut self) {
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}
// A request's callback and read buffer outlive cancellation until the final
// HANDLE_CLOSING callback. The UI is never a callback target.
mod asynchronous {
    use super::*;
    use std::sync::{Arc, Condvar, Mutex};
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Completion {
        Sent,
        Headers,
        Read(usize),
        Failed(u32),
    }
    struct Context {
        completion: Mutex<Option<Completion>>,
        changed: Condvar,
        buffer: Mutex<Box<[u8; 16 * 1024]>>,
    }
    #[cfg(test)]
    thread_local! { static CAPTURE: std::cell::RefCell<Option<Vec<std::sync::Weak<Context>>>> = const { std::cell::RefCell::new(None) }; }
    #[cfg(test)]
    pub(super) fn start_lifetime_probe() {
        CAPTURE.with(|capture| assert!(capture.replace(Some(Vec::new())).is_none()));
    }
    #[cfg(test)]
    pub(super) fn finish_lifetime_probe() -> impl Fn() -> bool {
        let captured = CAPTURE.with(|capture| capture.take().unwrap());
        assert!(!captured.is_empty());
        move || captured.iter().all(|context| context.upgrade().is_none())
    }
    pub(super) struct Request {
        handle: Internet,
        context: Arc<Context>,
    }
    impl Request {
        pub(super) fn new(handle: Internet) -> Result<Self, String> {
            let previous = unsafe {
                WinHttpSetStatusCallback(
                    handle.0,
                    Some(callback),
                    WINHTTP_CALLBACK_FLAG_SENDREQUEST_COMPLETE
                        | WINHTTP_CALLBACK_FLAG_HEADERS_AVAILABLE
                        | WINHTTP_CALLBACK_FLAG_READ_COMPLETE
                        | WINHTTP_CALLBACK_FLAG_REQUEST_ERROR
                        | WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING,
                    0,
                )
            };
            if previous.is_some_and(|callback| callback as usize == usize::MAX) {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let context = Arc::new(Context {
                completion: Mutex::new(None),
                changed: Condvar::new(),
                buffer: Mutex::new(Box::new([0; 16 * 1024])),
            });
            #[cfg(test)]
            CAPTURE.with(|capture| {
                if let Some(values) = &mut *capture.borrow_mut() {
                    values.push(Arc::downgrade(&context));
                }
            });
            let raw = Arc::into_raw(context.clone());
            let installed = unsafe {
                WinHttpSetOption(
                    Some(handle.0),
                    WINHTTP_OPTION_CONTEXT_VALUE,
                    Some(&(raw as usize).to_ne_bytes()),
                )
            };
            if let Err(error) = installed {
                // The failed option did not bind this context to the handle.
                unsafe {
                    drop(Arc::from_raw(raw));
                }
                return Err(error.to_string());
            }
            Ok(Self { handle, context })
        }
        pub(super) fn handle(&self) -> *mut c_void {
            self.handle.0
        }
        pub(super) fn context(&self) -> usize {
            Arc::as_ptr(&self.context) as usize
        }
        pub(super) fn wait(&mut self, budget: &Budget<'_>) -> Result<Completion, String> {
            let mut completion = crate::win_util::lock_or_recover(&self.context.completion);
            loop {
                budget.remaining_ms()?;
                if let Some(value) = completion.take() {
                    return match value {
                        Completion::Failed(error) => Err(format!("WinHTTP error {error}")),
                        value => Ok(value),
                    };
                }
                let timeout = Duration::from_millis((budget.remaining_ms()? as u64).min(100));
                let result = self
                    .context
                    .changed
                    .wait_timeout(completion, timeout)
                    .unwrap_or_else(|poison| poison.into_inner());
                completion = result.0;
            }
        }
        pub(super) fn expect(
            &mut self,
            expected: Completion,
            budget: &Budget<'_>,
        ) -> Result<(), String> {
            if self.wait(budget)? == expected {
                Ok(())
            } else {
                Err("unexpected WinHTTP completion".into())
            }
        }
        pub(super) fn read(
            &mut self,
            budget: &Budget<'_>,
            mut consume: impl FnMut(&[u8]) -> Result<(), String>,
        ) -> Result<usize, String> {
            let buffer = crate::win_util::lock_or_recover(&self.context.buffer).as_mut_ptr();
            // Only this worker starts reads. It does not access the buffer again
            // until READ_COMPLETE; the callback only publishes a count. On any
            // failure the closing callback retains the allocation until native
            // writes have finished, including unwind from a consumer panic.
            unsafe {
                WinHttpReadData(
                    self.handle.0,
                    buffer.cast(),
                    16 * 1024,
                    std::ptr::null_mut(),
                )
            }
            .map_err(|error| error.to_string())?;
            match self.wait(budget)? {
                Completion::Read(count) if count <= 16 * 1024 => {
                    let buffer = crate::win_util::lock_or_recover(&self.context.buffer);
                    consume(&buffer[..count])?;
                    Ok(count)
                }
                _ => Err("unexpected WinHTTP read completion".into()),
            }
        }
    }
    unsafe extern "system" fn callback(
        _: *mut c_void,
        raw: usize,
        status: u32,
        data: *mut c_void,
        length: u32,
    ) {
        if raw == 0 {
            return;
        }
        // WinHTTP guarantees HANDLE_CLOSING is the final notification. One Arc
        // belongs to the native context until that notification. Each callback
        // takes its own reference so concurrent callback bodies also stay valid.
        let pointer = raw as *const Context;
        unsafe {
            Arc::increment_strong_count(pointer);
        }
        let context = unsafe { Arc::from_raw(pointer) };
        if status == WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING {
            unsafe {
                drop(Arc::from_raw(pointer));
            }
            return;
        }
        let event = match status {
            WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE => Completion::Sent,
            WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE => Completion::Headers,
            WINHTTP_CALLBACK_STATUS_READ_COMPLETE => Completion::Read(length as usize),
            WINHTTP_CALLBACK_STATUS_REQUEST_ERROR
                if !data.is_null() && length as usize >= size_of::<WINHTTP_ASYNC_RESULT>() =>
            {
                let error = unsafe { data.cast::<WINHTTP_ASYNC_RESULT>().read_unaligned() };
                Completion::Failed(error.dwError)
            }
            _ => return,
        };
        let mut slot = crate::win_util::lock_or_recover(&context.completion);
        if slot.is_none() || matches!(event, Completion::Failed(_)) {
            *slot = Some(event);
        }
        context.changed.notify_one();
    }
}

struct Budget<'a> {
    started: Instant,
    duration: Duration,
    cancel: &'a crate::update_cancel::Token,
}
impl Budget<'_> {
    fn remaining_ms(&self) -> Result<i32, String> {
        self.cancel.check()?;
        let remaining = self
            .duration
            .checked_sub(self.started.elapsed())
            .filter(|value| !value.is_zero())
            .ok_or("update request exceeded time budget")?;
        // Zero means infinite to WinHTTP. Round a positive fraction up to 1ms.
        Ok(remaining.as_millis().clamp(1, i32::MAX as u128) as i32)
    }
    fn apply(&self, handle: *mut c_void) -> Result<(), String> {
        let remaining = self.remaining_ms()?;
        unsafe {
            WinHttpSetTimeouts(
                handle,
                remaining.min(5000),
                remaining.min(10000),
                remaining.min(10000),
                remaining.min(10000),
            )
        }
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
pub(crate) fn get(
    url: &str,
    limit: usize,
    budget: Duration,
    progress: impl Fn(usize),
) -> Result<Vec<u8>, String> {
    get_cancellable(
        url,
        limit,
        budget,
        progress,
        &crate::update_cancel::Token::default(),
    )
}
pub(crate) fn get_cancellable(
    url: &str,
    limit: usize,
    budget: Duration,
    progress: impl Fn(usize),
    cancel: &crate::update_cancel::Token,
) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    get_into_cancellable(
        url,
        limit,
        budget,
        progress,
        |chunk| {
            body.extend_from_slice(chunk);
            Ok(())
        },
        cancel,
    )?;
    Ok(body)
}
#[cfg(test)]
pub(crate) fn get_into(
    url: &str,
    limit: usize,
    budget: Duration,
    progress: impl Fn(usize),
    consume: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<usize, String> {
    get_into_cancellable(
        url,
        limit,
        budget,
        progress,
        consume,
        &crate::update_cancel::Token::default(),
    )
}
pub(crate) fn get_into_cancellable(
    url: &str,
    limit: usize,
    budget: Duration,
    progress: impl Fn(usize),
    mut consume: impl FnMut(&[u8]) -> Result<(), String>,
    cancel: &crate::update_cancel::Token,
) -> Result<usize, String> {
    let url = Url::parse(url)?;
    let budget = Budget {
        started: Instant::now(),
        duration: budget,
        cancel,
    };
    budget.remaining_ms()?;
    // Handle lifetimes are nested request -> connection -> session, all confined
    // to this worker. Windows retains certificate and hostname verification.
    unsafe {
        let agent = wide(concat!("minicon-winmgr/", env!("CARGO_PKG_VERSION")));
        let session = Internet::new(WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            if matches!(url.host.as_str(), "127.0.0.1" | "localhost") {
                WINHTTP_ACCESS_TYPE_NO_PROXY
            } else {
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY
            },
            None,
            None,
            WINHTTP_FLAG_ASYNC,
        ))?;
        budget.apply(session.0)?;
        let host = wide(&url.host);
        let connection = Internet::new(WinHttpConnect(
            session.0,
            PCWSTR(host.as_ptr()),
            url.port,
            0,
        ))?;
        let path = wide(&url.path);
        let request = Internet::new(WinHttpOpenRequest(
            connection.0,
            windows::core::w!("GET"),
            PCWSTR(path.as_ptr()),
            None,
            None,
            std::ptr::null(),
            if url.secure {
                WINHTTP_FLAG_SECURE
            } else {
                WINHTTP_OPEN_REQUEST_FLAGS(0)
            },
        ))?;
        let mut request = asynchronous::Request::new(request)?;
        WinHttpSetOption(
            Some(request.handle()),
            WINHTTP_OPTION_MAX_HTTP_AUTOMATIC_REDIRECTS,
            Some(&5u32.to_ne_bytes()),
        )
        .map_err(|e| e.to_string())?;
        WinHttpSetOption(
            Some(request.handle()),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP.to_ne_bytes()),
        )
        .map_err(|e| e.to_string())?;
        let headers = wide(if url.host == "api.github.com" {
            "Accept: application/vnd.github+json\r\n"
        } else {
            "Accept: */*\r\n"
        });
        budget.apply(request.handle())?;
        let remaining = budget.remaining_ms()? as u32;
        WinHttpSetOption(
            Some(request.handle()),
            WINHTTP_OPTION_RECEIVE_TIMEOUT,
            Some(&remaining.min(10000).to_ne_bytes()),
        )
        .map_err(|e| e.to_string())?;
        WinHttpSetOption(
            Some(request.handle()),
            WINHTTP_OPTION_RECEIVE_RESPONSE_TIMEOUT,
            Some(&remaining.to_ne_bytes()),
        )
        .map_err(|e| e.to_string())?;
        WinHttpSendRequest(
            request.handle(),
            Some(&headers[..headers.len() - 1]),
            None,
            0,
            0,
            request.context(),
        )
        .map_err(|e| e.to_string())?;
        request.expect(asynchronous::Completion::Sent, &budget)?;
        budget.apply(request.handle())?;
        // The receive timeout bounds a silent read. The response timeout also
        // limits headers that arrive slowly enough to keep individual reads alive.
        WinHttpSetOption(
            Some(request.handle()),
            WINHTTP_OPTION_RECEIVE_RESPONSE_TIMEOUT,
            Some(&(budget.remaining_ms()? as u32).to_ne_bytes()),
        )
        .map_err(|e| e.to_string())?;
        WinHttpReceiveResponse(request.handle(), std::ptr::null_mut())
            .map_err(|e| e.to_string())?;
        request.expect(asynchronous::Completion::Headers, &budget)?;
        let mut status = 0u32;
        let mut length = 4;
        WinHttpQueryHeaders(
            request.handle(),
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            None,
            Some((&mut status as *mut u32).cast()),
            &mut length,
            std::ptr::null_mut(),
        )
        .map_err(|e| e.to_string())?;
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        let mut declared = 0u32;
        let mut length = 4;
        let declared = WinHttpQueryHeaders(
            request.handle(),
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            None,
            Some((&mut declared as *mut u32).cast()),
            &mut length,
            std::ptr::null_mut(),
        )
        .is_ok()
        .then_some(declared as usize);
        if declared.is_some_and(|length| length > limit) {
            return Err("update response exceeds size limit".into());
        }
        let mut total = 0usize;
        loop {
            budget.apply(request.handle())?;
            let count = request.read(&budget, |chunk| {
                if total.saturating_add(chunk.len()) > limit {
                    return Err("update response exceeds size limit".into());
                }
                if !chunk.is_empty() {
                    consume(chunk)?;
                }
                Ok(())
            })?;
            budget.remaining_ms()?;
            if count == 0 {
                break;
            }
            total += count;
            progress(total);
        }
        if declared.is_some_and(|length| length != total) {
            return Err("truncated update response".into());
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_http_bounds_chunks_and_rejects_truncation_oversize_and_sink_failure() {
        use std::io::{Read, Write};
        for mode in [
            "ok",
            "oversize",
            "truncated",
            "sink",
            "unknown-length",
            "unknown-oversize",
            "budget",
        ] {
            let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/stream", server.local_addr().unwrap());
            let worker = std::thread::spawn(move || {
                let (mut stream, _) = server.accept().unwrap();
                let mut request = [0u8; 8192];
                assert!(stream.read(&mut request).unwrap() > 0);
                let header = if mode.starts_with("unknown-") {
                    "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".into()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        100_000
                    )
                };
                let _ = stream.write_all(header.as_bytes());
                if mode == "budget" {
                    std::thread::sleep(Duration::from_millis(60));
                }
                let total = if mode == "truncated" { 99_999 } else { 100_000 };
                let chunk = [b'a'; 4096];
                let mut written = 0;
                while written < total {
                    let length = chunk.len().min(total - written);
                    if stream.write_all(&chunk[..length]).is_err() {
                        break;
                    }
                    written += length;
                }
            });
            let mut consumed = 0;
            let result = get_into(
                &url,
                if matches!(mode, "oversize" | "unknown-oversize") {
                    1024
                } else {
                    100_000
                },
                if mode == "budget" {
                    Duration::from_millis(20)
                } else {
                    Duration::from_secs(3)
                },
                |_| {},
                |chunk| {
                    assert!(chunk.len() <= 16 * 1024);
                    assert!(chunk.iter().all(|byte| *byte == b'a'));
                    if mode == "sink" {
                        return Err("disk full".into());
                    }
                    consumed += chunk.len();
                    Ok(())
                },
            );
            worker.join().unwrap();
            if matches!(mode, "ok" | "unknown-length") {
                assert_eq!(result.unwrap(), 100_000);
                assert_eq!(consumed, 100_000);
            } else {
                assert!(result.is_err(), "{mode} unexpectedly succeeded");
                if mode == "oversize" {
                    assert_eq!(consumed, 0);
                }
                if mode == "sink" {
                    assert_eq!(result.unwrap_err(), "disk full");
                }
            }
        }
    }

    #[test]
    fn silent_and_trickling_responses_obey_remaining_budget_and_retry_succeeds() {
        use std::io::{Read, Write};
        for mode in [
            "silent-headers",
            "trickle-headers",
            "silent-body",
            "trickle-body",
            "disconnect",
        ] {
            let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/slow", server.local_addr().unwrap());
            let (release, stop) = std::sync::mpsc::channel();
            server.set_nonblocking(true).unwrap();
            let accepted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let served = accepted.clone();
            let worker = std::thread::spawn(move || {
                let started = Instant::now();
                let (mut stream, _) = loop {
                    match server.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if !matches!(
                                stop.recv_timeout(Duration::from_millis(5)),
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                            ) || started.elapsed() > Duration::from_secs(4)
                            {
                                return;
                            }
                        }
                        Err(error) => panic!("test accept failed: {error}"),
                    }
                };
                served.store(true, std::sync::atomic::Ordering::Relaxed);
                // Windows accepted sockets can inherit the listener's nonblocking
                // mode. Only accept polls; request I/O uses the bounded timeouts.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = [0u8; 8192];
                assert!(stream.read(&mut request).unwrap() > 0);
                match mode {
                    "silent-headers" => {
                        let _ = stop.recv_timeout(Duration::from_secs(4));
                    }
                    "trickle-headers" => {
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nX-Slow: ");
                        while matches!(
                            stop.recv_timeout(Duration::from_millis(20)),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                        ) {
                            if stream.write_all(b"a").is_err() {
                                break;
                            }
                        }
                    }
                    "silent-body" | "trickle-body" => {
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n");
                        if mode == "silent-body" {
                            let _ = stop.recv_timeout(Duration::from_secs(4));
                        } else {
                            while matches!(
                                stop.recv_timeout(Duration::from_millis(20)),
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                            ) {
                                if stream.write_all(b"a").is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    "disconnect" => {
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\npartial");
                    }
                    _ => unreachable!(),
                }
            });
            asynchronous::start_lifetime_probe();
            let started = Instant::now();
            let mut consumed = 0;
            let result = get_into(
                &url,
                100_000,
                Duration::from_millis(300),
                |_| {},
                |chunk| {
                    consumed += chunk.len();
                    Ok(())
                },
            );
            let elapsed = started.elapsed();
            let released = asynchronous::finish_lifetime_probe();
            let cleanup = Instant::now();
            while !released() && cleanup.elapsed() < Duration::from_secs(1) {
                std::thread::sleep(Duration::from_millis(5));
            }
            let closed_before_peer = released();
            let _ = release.send(());
            worker.join().unwrap();
            assert!(
                accepted.load(std::sync::atomic::Ordering::Relaxed),
                "{mode}: server never received the test request"
            );
            assert!(result.is_err(), "{mode}: unexpectedly succeeded");
            assert!(
                closed_before_peer,
                "{mode}: callback context/read buffer retained after cancellation"
            );
            assert!(
                elapsed < Duration::from_secs(3),
                "{mode}: elapsed {elapsed:?}"
            );
            if mode.ends_with("headers") {
                assert_eq!(consumed, 0);
            }
            // A new request after each native failure must work normally.
            let retry = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/retry", retry.local_addr().unwrap());
            let worker = std::thread::spawn(move || {
                let (mut stream, _) = retry.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = [0u8; 8192];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            });
            let result = get(&url, 2, Duration::from_secs(3), |_| {});
            worker.join().unwrap();
            assert_eq!(result.unwrap(), b"ok");
        }
    }

    #[test]
    fn consumer_unwind_releases_native_context_and_buffer() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/panic", listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = [0; 8192];
            let _ = stream.read(&mut bytes);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            );
        });
        asynchronous::start_lifetime_probe();
        let result = std::panic::catch_unwind(|| {
            get_into(
                &url,
                10,
                Duration::from_secs(3),
                |_| {},
                |_| panic!("synthetic consumer panic"),
            )
        });
        peer.join().unwrap();
        let released = asynchronous::finish_lifetime_probe();
        let started = Instant::now();
        while !released() && started.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(result.is_err());
        assert!(
            released(),
            "consumer panic retained native callback context/read buffer"
        );
    }

    #[test]
    fn cancellation_interrupts_inflight_requests_and_releases_callbacks_before_peer_close() {
        use std::io::{Read, Write};
        for headers in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/cancel", listener.local_addr().unwrap());
            let cancel = crate::update_cancel::Token::default();
            let signal = cancel.clone();
            let (release, stop) = std::sync::mpsc::channel();
            let peer = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = [0; 8192];
                assert!(stream.read(&mut bytes).unwrap() > 0);
                if headers {
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n",
                    );
                }
                signal.cancel();
                let _ = stop.recv_timeout(Duration::from_secs(4));
            });
            asynchronous::start_lifetime_probe();
            let started = Instant::now();
            let result = get_into_cancellable(
                &url,
                100000,
                Duration::from_secs(30),
                |_| {},
                |_| panic!("cancelled request must not consume data"),
                &cancel,
            );
            let elapsed = started.elapsed();
            let released = asynchronous::finish_lifetime_probe();
            let started = Instant::now();
            while !released() && started.elapsed() < Duration::from_secs(1) {
                std::thread::sleep(Duration::from_millis(5));
            }
            let closed_before_peer = released();
            let _ = release.send(());
            peer.join().unwrap();
            assert_eq!(result.unwrap_err(), "update operation cancelled");
            assert!(
                elapsed < Duration::from_secs(2),
                "cancel waited {elapsed:?}"
            );
            assert!(closed_before_peer);
        }
        let cancelled = crate::update_cancel::Token::default();
        cancelled.cancel();
        let result = get_cancellable(
            "http://127.0.0.1:1/cancelled",
            10,
            Duration::from_secs(30),
            |_| panic!("cancelled request must not report progress"),
            &cancelled,
        );
        assert_eq!(result.unwrap_err(), "update operation cancelled");
    }

    #[test]
    fn expired_budget_does_not_connect_or_call_consumers() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/expired", listener.local_addr().unwrap());
        assert!(
            get_into(
                &url,
                10,
                Duration::ZERO,
                |_| panic!("unexpected progress"),
                |_| panic!("unexpected consumer")
            )
            .is_err()
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn hashes_known_vectors_and_rejects_insecure_urls() {
        assert_eq!(
            sha256(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        for bad in [
            "http://github.com/a",
            "file:///a",
            "https://user@github.com/a",
            "https://github.com/a#x",
            "https://github.com/a\r\nX:y",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad}");
        }
        assert!(loopback("http://127.0.0.1:5123/latest"));
    }
    #[test]
    fn bounded_http_errors_and_success_against_loopback() {
        use std::io::{Read, Write};
        for (status, body, limit, success) in [
            (200, "hello", 5, true),
            (200, "too long", 3, false),
            (429, "limited", 30, false),
        ] {
            let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/test", server.local_addr().unwrap());
            let worker = std::thread::spawn(move || {
                let (mut stream, _) = server.accept().unwrap();
                let mut buf = [0; 8192];
                let _ = stream.read(&mut buf);
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let result = get(&url, limit, Duration::from_secs(3), |_| {});
            worker.join().unwrap();
            assert_eq!(result.is_ok(), success, "{result:?}");
        }
    }
}
