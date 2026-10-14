use std::process::ExitCode;

use javelin_native_packaging::{
    Args, Result, build, install, package, repro, smoke,
};

const USAGE: &str = "javelin-pack: development packaging for the native Javelin CLI

  build   --source DIR --out DIR [--source-date-epoch N] [--rustflag FLAG]
  package --build DIR --source DIR --source-sha SHA --out DIR [--source-date-epoch N]
  install --archive FILE --checksum FILE --dest NEWDIR
  smoke   --bin-dir DIR [--expect-sha256 HEX]
  repro   --source DIR --work DIR --report FILE [--source-date-epoch N]

Development use only. This tool never runs Git, publishes, or edits PATH.";

fn main() -> ExitCode {
    if let Some(code) = smoke::run_as_git_shim() {
        return ExitCode::from(code as u8);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("javelin-pack: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    let Some((command, rest)) = args.split_first() else {
        println!("{USAGE}");
        return Ok(());
    };
    match command.as_str() {
        "build" => {
            let a = Args::parse(rest, &["source", "out", "source-date-epoch", "rustflag"])?;
            let flags: Vec<String> = a.get("rustflag").into_iter().map(String::from).collect();
            let record = build::build(&a.path("source")?, &a.path("out")?, a.source_date_epoch()?, &flags)?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        "package" => {
            let a = Args::parse(rest, &["build", "source", "source-sha", "out", "source-date-epoch"])?;
            let p = package::package(
                &a.path("build")?,
                &a.path("source")?,
                a.get("source-sha").ok_or_else(|| javelin_native_packaging::Error("--source-sha is required".into()))?,
                &a.path("out")?,
            )?;
            println!(
                "{}",
                serde_json::json!({
                    "archive": p.archive, "checksum_file": p.checksum_file,
                    "archive_sha256": p.archive_sha256, "top_dir": p.top_dir,
                })
            );
        }
        "install" => {
            let a = Args::parse(rest, &["archive", "checksum", "dest"])?;
            let i = install::install(&a.path("archive")?, &a.path("checksum")?, &a.path("dest")?)?;
            println!(
                "{}",
                serde_json::json!({"dest": i.dest, "bin_dir": i.bin_dir, "binary_sha256": i.manifest.binary.sha256})
            );
        }
        "smoke" => {
            let a = Args::parse(rest, &["bin-dir", "expect-sha256"])?;
            let report = smoke::smoke(&a.path("bin-dir")?, a.get("expect-sha256"))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            smoke::fail_unless_ok(&report)?;
        }
        "repro" => {
            let a = Args::parse(rest, &["source", "work", "report", "source-date-epoch"])?;
            let report = repro::repro(&a.path("source")?, &a.path("work")?, a.source_date_epoch()?)?;
            let text = serde_json::to_string_pretty(&report)?;
            javelin_native_packaging::write(&a.path("report")?, text.as_bytes())?;
            println!("{text}");
        }
        other => javelin_native_packaging::bail!("unknown command {other:?}\n\n{USAGE}"),
    }
    Ok(())
}
