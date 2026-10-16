#![windows_subsystem = "windows"]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else { return };
    let output = format!("cwd={}\nargs={args:?}\n", std::env::current_dir().unwrap().display());
    let _ = std::fs::write(path, output);
}
