// Failing stand-in for a tool Javelin must never run (git, sudo, mount, ...). Copied once per
// tool name; it leaves `invoked-<name>` beside itself and exits 97.
fn main() {
    let exe = std::env::current_exe().expect("shim path");
    let name = exe.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_owned();
    let marker = exe.with_file_name(format!("invoked-{name}"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let _ = std::fs::write(marker, args.join(" "));
    eprintln!("{name} was invoked by a process under the gate-2 harness");
    std::process::exit(97);
}
