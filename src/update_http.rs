//! Bounded WinHTTP requests and CNG SHA-256, used only by update workers.
use crate::win_util::wide;
use std::ffi::c_void;
use std::time::{Duration, Instant};
use windows::Win32::{Networking::WinHttp::*, Security::Cryptography::*};
use windows::core::PCWSTR;

pub(crate) fn sha256(bytes: &[u8]) -> Result<String, String> {
    struct Algorithm(BCRYPT_ALG_HANDLE);
    impl Drop for Algorithm {
        fn drop(&mut self) {
            let _ = unsafe { BCryptCloseAlgorithmProvider(self.0, 0) };
        }
    }
    let mut handle = BCRYPT_ALG_HANDLE::default();
    unsafe {
        BCryptOpenAlgorithmProvider(
            &mut handle,
            BCRYPT_SHA256_ALGORITHM,
            None,
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
        .ok()
        .map_err(|e| e.to_string())?;
    }
    let algorithm = Algorithm(handle);
    let mut hash = [0; 32];
    unsafe { BCryptHash(algorithm.0, None, bytes, &mut hash) }
        .ok()
        .map_err(|e| e.to_string())?;
    Ok(hash.iter().map(|b| format!("{b:02x}")).collect())
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
pub(crate) fn get(
    url: &str,
    limit: usize,
    budget: Duration,
    progress: impl Fn(usize),
) -> Result<Vec<u8>, String> {
    let url = Url::parse(url)?;
    let started = Instant::now();
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
            0,
        ))?;
        WinHttpSetTimeouts(session.0, 5000, 10000, 10000, 10000).map_err(|e| e.to_string())?;
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
        WinHttpSetOption(
            Some(request.0),
            WINHTTP_OPTION_MAX_HTTP_AUTOMATIC_REDIRECTS,
            Some(&5u32.to_ne_bytes()),
        )
        .map_err(|e| e.to_string())?;
        WinHttpSetOption(
            Some(request.0),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP.to_ne_bytes()),
        )
        .map_err(|e| e.to_string())?;
        let headers = wide(if url.host == "api.github.com" {
            "Accept: application/vnd.github+json\r\n"
        } else {
            "Accept: */*\r\n"
        });
        WinHttpSendRequest(
            request.0,
            Some(&headers[..headers.len() - 1]),
            None,
            0,
            0,
            0,
        )
        .map_err(|e| e.to_string())?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| e.to_string())?;
        let mut status = 0u32;
        let mut length = 4;
        WinHttpQueryHeaders(
            request.0,
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
        let mut body = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            if started.elapsed() > budget {
                return Err("update request exceeded time budget".into());
            }
            let mut count = 0;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut count,
            )
            .map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            if body.len().saturating_add(count as usize) > limit {
                return Err("update response exceeds size limit".into());
            }
            body.extend_from_slice(&chunk[..count as usize]);
            progress(body.len());
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
