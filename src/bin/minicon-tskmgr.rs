//! Independent task-tool entry point; no Winmgr shell startup is invoked.
#![windows_subsystem = "windows"]

fn main() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new("--snapshot-json")) {
        let result = (|| -> Result<(), String> {
            let path = args.next().ok_or("Snapshot output path required")?;
            if args.next().is_some() {
                return Err("Unexpected snapshot arguments".into());
            }
            let mut collector = minicon_winmgr::task_service::Collector::new()?;
            let snapshot = collector.sample()?;
            let data = serde_json::to_vec_pretty(&snapshot).map_err(|e| e.to_string())?;
            std::fs::write(path, data).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            std::process::exit(1);
        }
    } else {
        // The native UI is implemented in the next development slice.
        std::process::exit(2);
    }
}
