//! One coalescing icon worker. No foreign window messages on the UI thread.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GCLP_HICON, GCLP_HICONSM, GetClassLongPtrW, GetWindowThreadProcessId, ICON_BIG, ICON_SMALL,
    ICON_SMALL2, PostMessageW, SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT, SendMessageTimeoutW,
    WM_GETICON,
};

use crate::win_util::lock_or_recover;
use crate::window_icon::SharedIcon;

pub(crate) const READY_MESSAGE: u32 = 0x8000 + 143;
const MAX_WINDOWS: usize = 256;
const BATCH_WAIT: Duration = Duration::from_millis(64);
const QUERY_MS: u32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Identity {
    hwnd: isize,
    pid: u32,
    thread: u32,
}
impl Identity {
    fn read(hwnd: HWND) -> Option<Self> {
        let mut pid = 0;
        let thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        (thread != 0 && pid != 0).then_some(Self {
            hwnd: hwnd.0 as isize,
            pid,
            thread,
        })
    }
    fn window(self) -> HWND {
        HWND(self.hwnd as *mut _)
    }
    fn still_current(self) -> bool {
        Self::read(self.window()) == Some(self)
    }
}
struct Request {
    generation: u64,
    windows: Vec<Identity>,
}
struct Response {
    generation: u64,
    icons: HashMap<Identity, SharedIcon>,
}
#[derive(Default)]
struct State {
    generation: u64,
    pending: Option<Request>,
    ready: Option<Response>,
    cache: HashMap<Identity, SharedIcon>,
    notified: bool,
    stopped: bool,
}
struct Worker {
    shared: Arc<(Mutex<State>, Condvar)>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}
static WORKER: OnceLock<Option<Worker>> = OnceLock::new();

impl Worker {
    fn start(notify: isize) -> std::io::Result<Self> {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let worker_state = shared.clone();
        let thread = std::thread::Builder::new()
            .name("window-icons".into())
            .spawn(move || {
                run(worker_state, notify);
            })?;
        Ok(Self {
            shared,
            thread: Mutex::new(Some(thread)),
        })
    }

    fn request(&self, mut windows: Vec<Identity>) {
        windows.truncate(MAX_WINDOWS);
        let keep: HashSet<_> = windows.iter().copied().collect();
        let mut state = lock_or_recover(&self.shared.0);
        if state.stopped {
            return;
        }
        state.generation = state.generation.wrapping_add(1);
        state.cache.retain(|key, _| keep.contains(key));
        state.ready = None;
        state.pending = (!windows.is_empty()).then_some(Request {
            generation: state.generation,
            windows,
        });
        self.shared.1.notify_one();
    }

    fn publish(&self) -> bool {
        let mut state = lock_or_recover(&self.shared.0);
        state.notified = false;
        if let Some(response) = state.ready.take()
            && !state.stopped
            && response.generation == state.generation
        {
            // request() already removed identities absent from this snapshot.
            // A timeout must not erase a still-valid owned icon from the last
            // successful query; only successful copies replace it.
            state.cache.extend(response.icons);
            true
        } else {
            false
        }
    }

    fn cached(&self, identity: Identity) -> SharedIcon {
        lock_or_recover(&self.shared.0)
            .cache
            .get(&identity)
            .cloned()
            .unwrap_or_default()
    }

    fn stop(&self) {
        {
            let mut state = lock_or_recover(&self.shared.0);
            state.stopped = true;
            state.pending = None;
            state.ready = None;
            state.cache.clear();
            self.shared.1.notify_one();
        }
        if let Some(thread) = lock_or_recover(&self.thread).take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(shared: Arc<(Mutex<State>, Condvar)>, notify: isize) {
    loop {
        let request = {
            let mut state = lock_or_recover(&shared.0);
            while !state.stopped && state.pending.is_none() {
                state = shared.1.wait(state).unwrap_or_else(|p| p.into_inner());
            }
            if state.stopped {
                return;
            }
            state.pending.take().unwrap()
        };
        let deadline = Instant::now() + BATCH_WAIT;
        let mut icons = HashMap::new();
        for identity in request.windows {
            {
                let state = lock_or_recover(&shared.0);
                if state.stopped || state.generation != request.generation {
                    break;
                }
            }
            if !identity.still_current() {
                continue;
            }
            let raw = query(identity, deadline);
            let icon = SharedIcon::copy_window_icon(raw);
            if icon.raw() != 0 && identity.still_current() {
                icons.insert(identity, icon);
            }
        }
        let mut state = lock_or_recover(&shared.0);
        if state.stopped {
            return;
        }
        if state.generation != request.generation {
            continue;
        }
        state.ready = Some(Response {
            generation: request.generation,
            icons,
        });
        // One outstanding posted message, even while the UI is busy. It will
        // consume the latest response rather than allocating one per refresh.
        if notify != 0 && !state.notified {
            state.notified = unsafe {
                PostMessageW(
                    Some(HWND(notify as *mut _)),
                    READY_MESSAGE,
                    WPARAM(0),
                    LPARAM(0),
                )
            }
            .is_ok();
        }
    }
}

fn query(identity: Identity, deadline: Instant) -> isize {
    for which in [ICON_SMALL2, ICON_SMALL, ICON_BIG] {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        if remaining == 0 {
            break;
        }
        let timeout = (remaining.min(QUERY_MS as u128)) as u32;
        let mut result = 0;
        let sent = unsafe {
            SendMessageTimeoutW(
                identity.window(),
                WM_GETICON,
                WPARAM(which as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
                timeout,
                Some(&mut result),
            )
        };
        if sent.0 == 0 {
            break;
        } // Do not repeat timeouts for the same window.
        if result != 0 {
            return result as isize;
        }
    }
    // Metadata fallback does not call the target window procedure. The worker
    // owns its separate queue and never attaches input queues to other threads.
    unsafe {
        let small = GetClassLongPtrW(identity.window(), GCLP_HICONSM);
        if small != 0 {
            small as isize
        } else {
            GetClassLongPtrW(identity.window(), GCLP_HICON) as isize
        }
    }
}

pub(crate) fn request(windows: &[crate::window_snapshot::WindowInfo]) {
    if windows.is_empty() && WORKER.get().is_none() {
        return;
    }
    let Some(worker) = WORKER.get_or_init(|| {
        Worker::start(crate::hub::shell_hwnd().map_or(0, |hwnd| hwnd.0 as isize))
            .map_err(|_| tracing::warn!("window icon worker unavailable; using text fallback"))
            .ok()
    }) else {
        return;
    };
    let identities = windows
        .iter()
        .take(MAX_WINDOWS)
        .filter_map(|window| Identity::read(window.hwnd))
        .collect();
    worker.request(identities);
}

pub(crate) fn cached(hwnd: HWND) -> SharedIcon {
    WORKER
        .get()
        .and_then(Option::as_ref)
        .zip(Identity::read(hwnd))
        .map_or_else(SharedIcon::default, |(worker, key)| worker.cached(key))
}

pub(crate) fn publish() -> bool {
    WORKER
        .get()
        .and_then(Option::as_ref)
        .is_some_and(Worker::publish)
}
pub(crate) fn shutdown() {
    if let Some(Some(worker)) = WORKER.get() {
        worker.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{PCWSTR, w};

    #[test]
    #[ignore = "isolated hidden-window experiment; run resources layer"]
    fn stalled_windows_coalesce_and_owned_results_outlive_the_source() {
        static ICON: AtomicIsize = AtomicIsize::new(0);
        static BLOCK: AtomicBool = AtomicBool::new(false);
        static STALLED: AtomicBool = AtomicBool::new(false);
        static RELEASE: AtomicBool = AtomicBool::new(false);
        static SOURCE_RELEASED: AtomicBool = AtomicBool::new(false);
        unsafe extern "system" fn source_proc(
            hwnd: HWND,
            msg: u32,
            wp: WPARAM,
            lp: LPARAM,
        ) -> LRESULT {
            if msg == WM_APP + 7 {
                unsafe {
                    SetClassLongPtrW(hwnd, GCLP_HICON, 0);
                    SetClassLongPtrW(hwnd, GCLP_HICONSM, 0);
                    let raw = ICON.swap(0, Ordering::SeqCst);
                    if raw != 0 {
                        let _ = DestroyIcon(HICON(raw as *mut _));
                    }
                    SOURCE_RELEASED.store(true, Ordering::SeqCst);
                }
                return LRESULT(0);
            }
            if msg == WM_GETICON {
                if BLOCK.load(Ordering::SeqCst) {
                    STALLED.store(true, Ordering::SeqCst);
                    while !RELEASE.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    STALLED.store(false, Ordering::SeqCst);
                }
                return LRESULT(ICON.load(Ordering::SeqCst));
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        fn wait(mut condition: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !condition() {
                assert!(
                    Instant::now() < deadline,
                    "hidden-window experiment timed out"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        fn valid(raw: isize) -> bool {
            use windows::Win32::Graphics::Gdi::{BITMAP, GetObjectW};
            let mut info = ICONINFO::default();
            let success = unsafe { GetIconInfo(HICON(raw as *mut _), &mut info) }.is_ok();
            let mut bitmap = BITMAP::default();
            let dimensions = !success
                || (!info.hbmColor.is_invalid()
                    && unsafe {
                        GetObjectW(
                            info.hbmColor.into(),
                            std::mem::size_of::<BITMAP>() as i32,
                            Some((&mut bitmap as *mut BITMAP).cast()),
                        )
                    } != 0
                    && bitmap.bmWidth == 32
                    && bitmap.bmHeight == 32);
            // GetIconInfo creates bitmaps; inspecting an icon must not leak them.
            unsafe {
                if !info.hbmColor.is_invalid() {
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(info.hbmColor.into());
                }
                if !info.hbmMask.is_invalid() {
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(info.hbmMask.into());
                }
            }
            assert!(
                dimensions,
                "window icon copies must use the requested bounded dimensions"
            );
            success
        }
        fn notifications(hwnd: HWND) -> usize {
            let mut count = 0;
            let mut msg = MSG::default();
            while unsafe {
                PeekMessageW(
                    &mut msg,
                    Some(hwnd),
                    READY_MESSAGE,
                    READY_MESSAGE,
                    PM_REMOVE,
                )
            }
            .as_bool()
            {
                count += 1;
            }
            count
        }
        struct Fixture {
            thread: u32,
            join: Option<std::thread::JoinHandle<()>>,
            notification: HWND,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                RELEASE.store(true, Ordering::SeqCst);
                unsafe {
                    let _ = PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0));
                    let _ = DestroyWindow(self.notification);
                }
                if let Some(join) = self.join.take() {
                    join.join().unwrap();
                }
            }
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let join = std::thread::spawn(move || unsafe {
            let icon = CopyIcon(LoadIconW(None, IDI_APPLICATION).unwrap()).unwrap();
            ICON.store(icon.0 as isize, Ordering::SeqCst);
            let class = w!("winmgr_hidden_icon_worker_fixture");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(source_proc),
                lpszClassName: class,
                hIcon: icon,
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&wc), 0);
            let windows: Vec<_> = (0..32)
                .map(|_| {
                    CreateWindowExW(
                        Default::default(),
                        class,
                        PCWSTR::null(),
                        WS_OVERLAPPEDWINDOW,
                        0,
                        0,
                        32,
                        32,
                        None,
                        None,
                        None,
                        None,
                    )
                    .unwrap()
                })
                .collect();
            tx.send((
                windows
                    .iter()
                    .map(|hwnd| Identity::read(*hwnd).unwrap())
                    .collect::<Vec<_>>(),
                windows::Win32::System::Threading::GetCurrentThreadId(),
            ))
            .unwrap();
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                DispatchMessageW(&msg);
            }
            for hwnd in windows {
                if IsWindow(Some(hwnd)).as_bool() {
                    let _ = DestroyWindow(hwnd);
                }
            }
            let _ = UnregisterClassW(class, None);
            let raw = ICON.swap(0, Ordering::SeqCst);
            if raw != 0 {
                let _ = DestroyIcon(HICON(raw as *mut _));
            }
        });
        let (identities, thread) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let notification = unsafe {
            CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
            .unwrap()
        };
        let _fixture = Fixture {
            thread,
            join: Some(join),
            notification,
        };
        let baseline = crate::metrics::sample();
        let worker = Worker::start(notification.0 as isize).unwrap();
        worker.request(identities.clone());
        wait(|| lock_or_recover(&worker.shared.0).ready.is_some());
        assert_eq!(notifications(notification), 1);
        assert!(worker.publish());
        assert_eq!(
            lock_or_recover(&worker.shared.0).cache.len(),
            32,
            "the populated-memory sample requires all 32 owned images"
        );
        let held = worker.cached(identities[0]);
        assert_ne!(held.raw(), 0);
        assert_ne!(held.raw(), ICON.load(Ordering::SeqCst), "must own a copy");
        assert!(valid(held.raw()));
        let populated = crate::metrics::sample();

        // Leave the notification queue undrained, then finish another batch.
        worker.request(identities.clone());
        wait(|| lock_or_recover(&worker.shared.0).ready.is_some());
        worker.request(identities.clone());
        wait(|| lock_or_recover(&worker.shared.0).ready.is_some());
        assert_eq!(
            notifications(notification),
            1,
            "posted messages must coalesce too"
        );
        assert!(worker.publish());

        BLOCK.store(true, Ordering::SeqCst);
        worker.request(identities.clone());
        wait(|| STALLED.load(Ordering::SeqCst));
        let started = Instant::now();
        for _ in 0..1000 {
            worker.request(identities.clone());
        }
        // The newest snapshot removes one window. An older completed response
        // must not bring its icon back, even though its HWND is still live.
        worker.request(identities[..31].to_vec());
        let submit_ms = started.elapsed().as_millis();
        assert!(
            submit_ms < 500,
            "request path waited for the stalled source"
        );
        {
            let state = lock_or_recover(&worker.shared.0);
            assert!(
                state
                    .pending
                    .as_ref()
                    .is_none_or(|r| r.windows.len() <= MAX_WINDOWS)
            );
        }
        let started = Instant::now();
        wait(|| worker.publish());
        let settle_ms = started.elapsed().as_millis();
        assert!(settle_ms < 1000, "timeouts accumulated across windows");
        assert_eq!(lock_or_recover(&worker.shared.0).cache.len(), 31);
        assert_eq!(
            worker.cached(identities[31]).raw(),
            0,
            "stale result restored a removed target"
        );
        assert_ne!(
            worker.cached(identities[0]).raw(),
            0,
            "class fallback remains available"
        );
        let retained = worker.cached(identities[0]);
        let (class_big, class_small) = unsafe {
            (
                SetClassLongPtrW(identities[0].window(), GCLP_HICON, 0),
                SetClassLongPtrW(identities[0].window(), GCLP_HICONSM, 0),
            )
        };
        worker.request(vec![identities[0]]);
        wait(|| worker.publish());
        assert_eq!(
            worker.cached(identities[0]).raw(),
            retained.raw(),
            "a failed query must preserve the last owned icon for a current target"
        );
        unsafe {
            SetClassLongPtrW(identities[0].window(), GCLP_HICON, class_big as isize);
            SetClassLongPtrW(identities[0].window(), GCLP_HICONSM, class_small as isize);
        }
        drop(retained);
        // Reproduce the previous native call pattern on the test thread,
        // never on the user's shell: three 50ms waits for each of five windows.
        let started = Instant::now();
        for identity in identities.iter().take(5) {
            for which in [ICON_SMALL2, ICON_SMALL, ICON_BIG] {
                let mut result = 0;
                unsafe {
                    SendMessageTimeoutW(
                        identity.window(),
                        WM_GETICON,
                        WPARAM(which as usize),
                        LPARAM(0),
                        SMTO_ABORTIFHUNG,
                        50,
                        Some(&mut result),
                    );
                }
            }
        }
        let legacy_ms = started.elapsed().as_millis();
        assert!(
            legacy_ms > settle_ms,
            "native reference did not reproduce accumulated waits"
        );
        RELEASE.store(true, Ordering::SeqCst);
        wait(|| !STALLED.load(Ordering::SeqCst));
        let source_raw = ICON.load(Ordering::SeqCst);
        unsafe {
            PostMessageW(
                Some(identities[1].window()),
                WM_APP + 7,
                WPARAM(0),
                LPARAM(0),
            )
            .unwrap();
        }
        wait(|| SOURCE_RELEASED.load(Ordering::SeqCst));
        assert!(
            !valid(source_raw),
            "fixture must really release its original icon"
        );
        assert!(
            valid(held.raw()),
            "owned snapshot must survive original icon destruction"
        );
        unsafe {
            PostMessageW(Some(identities[0].window()), WM_CLOSE, WPARAM(0), LPARAM(0)).unwrap();
        }
        wait(|| !identities[0].still_current());
        assert!(
            valid(held.raw()),
            "owned snapshot must survive source destruction"
        );
        let raw = held.raw();
        drop(held);
        assert!(!valid(raw), "last reference must release the owned icon");
        worker.request(Vec::new());
        assert!(lock_or_recover(&worker.shared.0).cache.is_empty());
        worker.stop();
        assert!(lock_or_recover(&worker.thread).is_none());
        assert!(lock_or_recover(&worker.shared.0).pending.is_none());
        let released = crate::metrics::sample();
        let private_delta = populated.private_bytes as i64 - baseline.private_bytes as i64;
        let retained_delta = released.private_bytes as i64 - baseline.private_bytes as i64;
        let user_delta = released.user_objects as i64 - baseline.user_objects as i64;
        let gdi_delta = released.gdi_objects as i64 - baseline.gdi_objects as i64;
        println!(
            "ICON_WORKER_SAMPLE {{\"windows\":32,\"latest_windows\":31,\"requests\":1000,\"submit_ms\":{submit_ms},\"settle_ms\":{settle_ms},\"legacy_five_windows_ms\":{legacy_ms},\"populated_private_delta_bytes\":{private_delta},\"released_private_delta_bytes\":{retained_delta},\"released_user_delta\":{user_delta},\"released_gdi_delta\":{gdi_delta}}}"
        );
    }
}
