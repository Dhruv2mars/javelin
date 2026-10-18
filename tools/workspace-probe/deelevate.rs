// Windows only: run a command as a standard user. Hosted runners give the job an elevated
// administrator token; this derives the SAFER "Normal User" token from it (Administrators
// becomes deny-only, administrator privileges are removed), lowers it to Medium integrity,
// and runs the command with the same stdio, waiting for its exit code.
// Build: rustc -O deelevate.rs. Usage: deelevate PROGRAM [ARGS...]
#![cfg(windows)]
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;

type Handle = *mut c_void;
#[repr(C)]
struct SidAndAttributes { sid: *mut c_void, attributes: u32 }
#[repr(C)]
struct StartupInfo {
    cb: u32, reserved: *mut u16, desktop: *mut u16, title: *mut u16,
    x: u32, y: u32, x_size: u32, y_size: u32, x_chars: u32, y_chars: u32, fill: u32,
    flags: u32, show_window: u16, reserved2_len: u16, reserved2: *mut u8,
    stdin: Handle, stdout: Handle, stderr: Handle,
}
#[repr(C)]
struct ProcessInfo { process: Handle, thread: Handle, pid: u32, tid: u32 }

#[link(name = "advapi32")]
unsafe extern "system" {
    fn SaferCreateLevel(scope: u32, level: u32, flags: u32, out: *mut Handle, reserved: *mut c_void) -> i32;
    fn SaferComputeTokenFromLevel(level: Handle, token: Handle, out: *mut Handle, flags: u32, reserved: *mut c_void) -> i32;
    fn ConvertStringSidToSidW(sid: *const u16, out: *mut *mut c_void) -> i32;
    fn GetLengthSid(sid: *mut c_void) -> u32;
    fn SetTokenInformation(token: Handle, class: u32, info: *const c_void, len: u32) -> i32;
    fn CreateProcessAsUserW(token: Handle, app: *const u16, cmd: *mut u16, pa: *mut c_void, ta: *mut c_void,
        inherit: i32, flags: u32, env: *mut c_void, cwd: *const u16, si: *mut StartupInfo, pi: *mut ProcessInfo) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetStdHandle(which: u32) -> Handle;
    fn SetHandleInformation(h: Handle, mask: u32, flags: u32) -> i32;
    fn WaitForSingleObject(h: Handle, ms: u32) -> u32;
    fn GetExitCodeProcess(h: Handle, code: *mut u32) -> i32;
}

fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain([0]).collect() }
fn check(ok: i32, what: &str) {
    if ok == 0 { eprintln!("deelevate: {what} failed: {}", std::io::Error::last_os_error()); std::process::exit(125) }
}
// CommandLineToArgvW quoting.
fn quote(arg: &std::ffi::OsStr) -> Vec<u16> {
    let a: Vec<u16> = arg.encode_wide().collect();
    if !a.is_empty() && !a.iter().any(|&c| c == b' ' as u16 || c == b'\t' as u16 || c == b'"' as u16) { return a }
    let mut out = vec![b'"' as u16];
    let mut slashes = 0;
    for &c in &a {
        if c == b'\\' as u16 { slashes += 1; continue }
        if c == b'"' as u16 { slashes = slashes * 2 + 1 }
        out.extend(std::iter::repeat_n(b'\\' as u16, slashes));
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    out
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() { eprintln!("usage: deelevate PROGRAM [ARGS...]"); std::process::exit(125) }
    let mut cmd: Vec<u16> = args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(&(b' ' as u16));
    cmd.push(0);
    unsafe {
        let (mut level, mut token, mut sid) = (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        // SAFER_SCOPEID_USER, SAFER_LEVELID_NORMALUSER, SAFER_LEVEL_OPEN
        check(SaferCreateLevel(2, 0x20000, 1, &mut level, std::ptr::null_mut()), "SaferCreateLevel");
        check(SaferComputeTokenFromLevel(level, std::ptr::null_mut(), &mut token, 0, std::ptr::null_mut()), "SaferComputeTokenFromLevel");
        check(ConvertStringSidToSidW(wide("S-1-16-8192").as_ptr(), &mut sid), "ConvertStringSidToSid"); // Medium
        let label = SidAndAttributes { sid, attributes: 0x20 }; // SE_GROUP_INTEGRITY
        let len = (std::mem::size_of::<SidAndAttributes>() as u32) + GetLengthSid(sid);
        check(SetTokenInformation(token, 25, &label as *const _ as *const c_void, len), "SetTokenInformation(TokenIntegrityLevel)");
        let mut si: StartupInfo = std::mem::zeroed();
        si.cb = std::mem::size_of::<StartupInfo>() as u32;
        si.flags = 0x100; // STARTF_USESTDHANDLES
        for (slot, which) in [(&mut si.stdin, -10i32 as u32), (&mut si.stdout, -11i32 as u32), (&mut si.stderr, -12i32 as u32)] {
            *slot = GetStdHandle(which);
            SetHandleInformation(*slot, 1, 1); // HANDLE_FLAG_INHERIT
        }
        let mut pi: ProcessInfo = std::mem::zeroed();
        check(CreateProcessAsUserW(token, std::ptr::null(), cmd.as_mut_ptr(), std::ptr::null_mut(), std::ptr::null_mut(),
            1, 0, std::ptr::null_mut(), std::ptr::null(), &mut si, &mut pi), "CreateProcessAsUser");
        WaitForSingleObject(pi.process, u32::MAX);
        let mut code = 1u32;
        check(GetExitCodeProcess(pi.process, &mut code), "GetExitCodeProcess");
        std::process::exit(code as i32);
    }
}
