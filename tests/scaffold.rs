use std::process::Command;

#[test]
fn initial_scaffold_baseline() {
    let result = Command::new(env!("CARGO_BIN_EXE_javelin"))
        .arg("init")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"Hello, world!\n");
    assert!(result.stderr.is_empty());
}
