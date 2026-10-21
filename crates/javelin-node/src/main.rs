//! `javelin-node serve <dir> <name> <addr>` hosts the World log in `dir` as repository
//! `name` on a loopback address such as `127.0.0.1:7878`.

use std::{error::Error, process::ExitCode};

use javelin_node::{FileWorldLog, bind_loopback, serve};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
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
    let listener = bind_loopback(addr)?;
    eprintln!("javelin-node serving {name} on {}", listener.local_addr()?);
    serve(&listener, name, &mut log, None)?;
    Ok(())
}
