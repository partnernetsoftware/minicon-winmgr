#![windows_subsystem = "windows"]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> isize;
    fn GetCurrentProcessId() -> u32;
    fn CloseHandle(handle: isize) -> i32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: isize, access: u32, token: *mut isize) -> i32;
    fn GetTokenInformation(token: isize, kind: i32, value: *mut core::ffi::c_void, size: u32, needed: *mut u32) -> i32;
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else { return };
    let mut token = 0isize;
    let mut elevated = 0u32;
    let mut needed = 0u32;
    unsafe {
        assert_ne!(OpenProcessToken(GetCurrentProcess(), 8, &mut token), 0);
        let result = GetTokenInformation(token, 20, (&mut elevated as *mut u32).cast(), 4, &mut needed);
        CloseHandle(token);
        assert_ne!(result, 0);
    }
    let output = format!("pid={}\nelevated={}\ncwd={}\nargs={args:?}\n", unsafe {GetCurrentProcessId()}, elevated != 0, std::env::current_dir().unwrap().display());
    std::fs::write(path, output).unwrap();
}
