//! Independent task-tool entry point; no Winmgr shell startup is invoked.
#![windows_subsystem = "windows"]

fn main() {
    let mut args = std::env::args_os().skip(1).peekable();
    if args.peek().is_some_and(|a| a == "--notes-json") {
        args.next();
        unsafe {
            let _ = windows::Win32::System::Console::AttachConsole(
                windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
        std::process::exit(minicon_winmgr::task_notes_cli::run(args.collect()));
    }
    if args
        .peek()
        .is_some_and(|a| a == "--help" || a == "-h" || a == "--version")
    {
        unsafe {
            let _ = windows::Win32::System::Console::AttachConsole(
                windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
        println!(
            "minicon-tskmgr {}\n--language en|zh --theme light|dark --focus-pid PID --focus-created FILETIME\n--search TEXT --network --tree | --flat\n--snapshot-json PATH\n--notes-json [--store ABSOLUTE_DIRECTORY] (one JSON request on stdin)\n--notes-store ABSOLUTE_DIRECTORY (ordinary UI only)\nUse the UI All users entry for administrator authorization.",
            env!("CARGO_PKG_VERSION")
        );
        return;
    }
    if args.peek().is_some_and(|a| a == "--snapshot-json") {
        args.next();
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
        let result = (|| -> Result<(), String> {
            let mut options = minicon_winmgr::task_ui::Options::default();
            if let Some(home) = std::env::var_os("USERPROFILE") {
                use std::io::Read;
                if let Ok(file) =
                    std::fs::File::open(std::path::PathBuf::from(home).join("winmgr.cnf"))
                {
                    let mut text = String::new();
                    let _ = file.take(65536).read_to_string(&mut text);
                    for line in text.lines() {
                        if let Some((key, value)) = line.split_once('=') {
                            match key.trim() {
                                "ui.language" => options.chinese = value.trim() == "zh",
                                "appearance.mode" => options.light = value.trim() == "light",
                                _ => (),
                            }
                        }
                    }
                }
            }
            let mut pid = None;
            let mut created = None;
            let mut all = false;
            let mut pipe = None;
            let mut origin_pid = None;
            let mut origin_created = None;
            while let Some(argument) = args.next() {
                match argument.to_str(){
                    Some("--all-users")=>all=true,
                    Some("--origin-pipe")=>pipe=Some(args.next().and_then(|v|v.into_string().ok()).ok_or("Expected local caller reference")?),
                    Some("--origin-pid")=>origin_pid=Some(args.next().and_then(|v|v.into_string().ok()).and_then(|v|v.parse::<u32>().ok()).ok_or("Expected caller PID")?),
                    Some("--origin-created")=>origin_created=Some(args.next().and_then(|v|v.into_string().ok()).and_then(|v|v.parse::<u64>().ok()).ok_or("Expected caller creation time")?),
                    Some("--network")=>options.network=true,
                    Some("--tree")=>options.tree=Some(true),
                    Some("--flat")=>options.tree=Some(false),
                    Some("--notes-store")=>options.notes_store=Some(std::path::PathBuf::from(args.next().ok_or("Expected notes directory")?)),
                    Some("--search")=>{ options.search=args.next().and_then(|v|v.into_string().ok()).ok_or("Expected search text")?; if options.search.encode_utf16().count()>512 || options.search.contains('\0') {return Err("Search text is too long or invalid".into());} },
                    Some("--language")=>{options.chinese=match args.next().and_then(|v|v.into_string().ok()).as_deref(){Some("zh")=>true,Some("en")=>false,_=>return Err("Expected --language en or zh".into())};},
                    Some("--theme")=>{options.light=match args.next().and_then(|v|v.into_string().ok()).as_deref(){Some("light")=>true,Some("dark")=>false,_=>return Err("Expected --theme light or dark".into())};},
                    Some("--focus-pid")=>pid=Some(args.next().and_then(|v|v.into_string().ok()).and_then(|v|v.parse::<u32>().ok()).ok_or("Expected process ID")?),
                    Some("--focus-created")=>created=Some(args.next().and_then(|v|v.into_string().ok()).and_then(|v|v.parse::<u64>().ok()).ok_or("Expected creation time")?),
                    _=>return Err("Arguments: --language en|zh --theme light|dark --focus-pid PID --focus-created FILETIME; or --snapshot-json PATH".into()),
                }
            }
            if all {
                if options.notes_store.is_some() {
                    return Err("Custom notes stores require the ordinary task entry".into());
                }
                let reference =
                    pipe.ok_or("Open all tasks through the explicit administrator entry")?;
                let identity = minicon_winmgr::task_service::Identity {
                    pid: origin_pid.ok_or("Missing verified caller PID")?,
                    created_100ns: origin_created.ok_or("Missing caller creation time")?,
                };
                // Authorization and peer verification precede all instance/UI work.
                options.access = Some(
                    minicon_winmgr::task_access::Access::administrator_from_origin(
                        &reference, identity,
                    )?,
                );
            } else if pipe.is_some() || origin_pid.is_some() || origin_created.is_some() {
                return Err("Caller references require the explicit administrator role".into());
            }
            if created.is_some() && pid.is_none() {
                return Err("Creation time requires a target PID".into());
            }
            if let Some(pid) = pid {
                options.focus = Some(if let Some(created_100ns) = created {
                    minicon_winmgr::task_service::Identity { pid, created_100ns }
                } else {
                    match &options.access {
                        Some(access) => access.identify(pid)?,
                        None => minicon_winmgr::task_service::identify(pid)?,
                    }
                });
            }
            minicon_winmgr::task_ui::run(options)
        })();
        if let Err(error) = result {
            use windows::{Win32::UI::WindowsAndMessaging::*, core::PCWSTR};
            let text: Vec<u16> = error.encode_utf16().chain(Some(0)).collect();
            let title: Vec<u16> = "minicon-tskmgr".encode_utf16().chain(Some(0)).collect();
            unsafe {
                MessageBoxW(
                    None,
                    PCWSTR(text.as_ptr()),
                    PCWSTR(title.as_ptr()),
                    MB_OK | MB_ICONERROR,
                );
            }
            std::process::exit(1);
        }
    }
}
