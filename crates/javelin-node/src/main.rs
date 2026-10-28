//! `javelin-node serve <dir> <name> <addr>` hosts the World log in `dir` as repository
//! `name` on a loopback address such as `127.0.0.1:7878`. Requests are authorized against
//! the keys and permissions the control plane stored in `dir` (`auth.rs`).
//! `javelin-node --version` prints the package version, as `javelin --version` does.

use std::{error::Error, process::ExitCode};

use javelin_node::{FileWorldLog, auth::Gate, bind_loopback, serve};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--version" | "-V"] => {
            println!("javelin-node {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        ["serve", dir, name, addr] => run(dir, name, addr),
        _ => Err("usage: javelin-node serve <dir> <name> <loopback addr>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(dir: &str, name: &str, addr: &str) -> Result<(), Box<dyn Error>> {
    let mut log = FileWorldLog::open(dir)?;
    let gate = Gate::load(dir.as_ref(), name)?;
    let listener = bind_loopback(addr)?;
    eprintln!("javelin-node serving {name} on {}", listener.local_addr()?);
    serve(&listener, name, &mut log, &gate, None)?;
    Ok(())
}
