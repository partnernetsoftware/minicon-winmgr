# minicon-winmgr
Rust Windows shell / session manager (Cairo Desktop-class).
## Motivation
- Stock `explorer.exe` is often unstable for long-lived company desktop sessions.
- [Cairo Desktop](https://github.com/cairoshell/cairoshell) improves the shell experience but still frequently freezes in our environment.
- This project rebuilds a Windows shell in **Rust**, and adds **company-oriented Windows session management** on top of a Cairo-class desktop shell.
## Direction
- Win-shell replacement / companion focused on reliability and session lifecycle.
- Session management for company workstations (start, recover, isolate, audit-friendly operations).
- Iterate as a normal git repo; **no release packaging or code signing** in early phases.
## Status
Skeleton only. Not production-ready. Remote GitHub create/push pending valid `gh` auth on this host.
## Build
```bat
set RUSTUP_HOME=C:\Users\wjc2022\.rustup
set CARGO_HOME=C:\Users\wjc2022\.cargo
set PATH=C:\Users\wjc2022\.cargo\bin;%PATH%
cargo build
```
