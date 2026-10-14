use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use macos_sparse_backend::{Overlay, clone_upper};
use nfsserve::tcp::{NFSTcp, NFSTcpListener};

const USAGE: &str = "usage:\n  macos-sparse-backend serve --base DIR --upper DIR --port N\n  macos-sparse-backend clone --from UPPER --to UPPER";

fn arg(args: &[String], flag: &str) -> Option<PathBuf> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
}

/// Holds an exclusive lock on the Layer's private directory while it is served,
/// so a clone can refuse a Layer that is not quiescent.
fn layer_lock(upper: &Path) -> std::io::Result<File> {
    std::fs::create_dir_all(upper)?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(upper.join("lock"))?;
    f.try_lock().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!("Layer is busy: {e}"),
        )
    })?;
    Ok(f)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("serve") => serve(&args),
        Some("clone") => clone(&args),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}

fn clone(args: &[String]) -> Result<(), String> {
    let (Some(from), Some(to)) = (arg(args, "--from"), arg(args, "--to")) else {
        return Err(USAGE.into());
    };
    let _held = layer_lock(&from).map_err(|e| format!("clone refused: {e}"))?;
    clone_upper(&from, &to).map_err(|e| format!("clone failed: {e}"))?;
    println!("cloned {} -> {}", from.display(), to.display());
    Ok(())
}

fn serve(args: &[String]) -> Result<(), String> {
    let (Some(base), Some(upper), Some(port)) = (
        arg(args, "--base"),
        arg(args, "--upper"),
        arg(args, "--port"),
    ) else {
        return Err(USAGE.into());
    };
    let port: u16 = port
        .to_str()
        .and_then(|p| p.parse().ok())
        .ok_or("invalid --port")?;
    let _held = layer_lock(&upper).map_err(|e| format!("serve refused: {e}"))?;
    let overlay = Overlay::open(&base, &upper).map_err(|e| format!("open failed: {e}"))?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let listener = NFSTcpListener::bind(&format!("127.0.0.1:{port}"), overlay)
            .await
            .map_err(|e| e.to_string())?;
        println!("listening 127.0.0.1:{}", listener.get_listen_port());
        tokio::select! {
            r = listener.handle_forever() => r.map_err(|e| e.to_string()),
            _ = tokio::signal::ctrl_c() => Ok(()),
        }
    })
}
