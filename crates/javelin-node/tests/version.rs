use std::process::Command;

#[test]
fn version_flag_prints_the_package_version() {
    for flag in ["--version", "-V"] {
        let output = Command::new(env!("CARGO_BIN_EXE_javelin-node"))
            .arg(flag)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("javelin-node {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}
