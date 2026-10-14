//! Release-mode measurement of the native Javelin storage through its public API.
//!
//! native-bench run --profile smoke|ci|local --seed N --out DIR [--workdir DIR]
//!     [--budget-secs N] [--source-commit SHA] [--keep-repos] [--allow-debug-build]
//! native-bench verify DIR
mod contention;
mod ctx;
mod fixture;
mod host;
mod orchestrate;
mod params;
mod record;
mod scenarios;
mod verify;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => orchestrate::run(&args).map(|ok| if ok { 0 } else { 1 }),
        Some("verify") => match args.get(1) {
            Some(dir) => {
                orchestrate::verify_and_print(dir.as_ref()).map(|ok| if ok { 0 } else { 1 })
            }
            None => Err("usage: native-bench verify DIR".into()),
        },
        Some("scenario") => orchestrate::scenario(&args).map(|_| 0),
        Some("worker") => contention::worker(&args).map(|_| 0),
        _ => Err(
            "usage: native-bench run --out DIR [--profile smoke|ci|local] [--seed N] | verify DIR"
                .into(),
        ),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
