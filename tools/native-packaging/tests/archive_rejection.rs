//! Valid-checksum regressions for the hub's raw archive audit.
use std::{fs, io::Write, path::Path, process::Command};

use sha2::{Digest, Sha256};

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn base_files() -> Vec<(String, Vec<u8>)> {
    let bin = if cfg!(windows) {
        "javelin.exe"
    } else {
        "javelin"
    };
    let binary = b"fixture binary bytes";
    let manifest = serde_json::json!({
        "schema": 1, "channel": "development", "pre_ga": true,
        "notice": "fixture", "name": "javelin", "cargo_version": "0.1.0",
        "source": {"sha": "a".repeat(40), "cargo_lock_sha256": "b".repeat(64),
            "source_files_sha256": "c".repeat(64), "source_files": 4},
        "toolchain": {"rustc_verbose": "fixture", "cargo": "fixture", "build_command": [],
            "rustflags": [], "source_date_epoch": 0},
        "platform": {"known": {"os": "fixture", "arch": "fixture", "target_triple": "fixture"},
            "minimum_os_version": "unconfirmed", "other_architectures": "unconfirmed"},
        "binary": {"path": format!("bin/{bin}"), "sha256": sha(binary),
            "size": binary.len(), "test_hook_markers_absent": []},
        "dependencies": []
    });
    vec![
        (format!("top/bin/{bin}"), binary.to_vec()),
        ("top/LICENSE".into(), b"license".to_vec()),
        (
            "top/PACKAGE.json".into(),
            serde_json::to_vec(&manifest).unwrap(),
        ),
    ]
}

fn zip(extra: Option<(&str, u64)>) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in base_files() {
        writer.start_file(name, options).unwrap();
        writer.write_all(&data).unwrap();
    }
    if let Some((name, size)) = extra {
        writer.start_file(name, options).unwrap();
        std::io::copy(&mut std::io::repeat(b'x').take(size), &mut writer).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

use std::io::Read;

/// Mutate a generated central record, with offsets supplied by the ZIP library.
fn set_zip_mode(bytes: &mut [u8], name: &str, mode: u32, host: u8) {
    let archive = zip::ZipArchive::new(std::io::Cursor::new(&*bytes)).unwrap();
    let entry = archive
        .by_index_data(archive.index_for_name(name).unwrap())
        .unwrap();
    let offset = entry.central_header_start() as usize;
    bytes[offset + 5] = host;
    bytes[offset + 38..offset + 42].copy_from_slice(&(mode << 16).to_le_bytes());
}

fn tar(sparse: bool) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in base_files() {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        builder
            .append_data(&mut header, name, data.as_slice())
            .unwrap();
    }
    if sparse {
        builder
            .append_pax_extensions([
                ("GNU.sparse.map", b"0,4".as_slice()),
                ("GNU.sparse.size", b"536870912".as_slice()),
                ("GNU.sparse.name", b"top/sparse".as_slice()),
            ])
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        builder
            .append_data(&mut header, "top/sparse", b"data".as_slice())
            .unwrap();
    }
    let mut raw = builder.into_inner().unwrap();
    // tar stops at its zero blocks before the gzip trailer. Extra zero padding is
    // valid tar and reproduces the unread trailer in the real macOS package.
    raw.resize(raw.len() + 16 * 1024, 0);
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&raw).unwrap();
    encoder.finish().unwrap()
}

fn snapshot(root: &Path) -> Vec<(std::path::PathBuf, Option<Vec<u8>>)> {
    fn walk(root: &Path, path: &Path, result: &mut Vec<(std::path::PathBuf, Option<Vec<u8>>)>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                result.push((path.strip_prefix(root).unwrap().into(), None));
                walk(root, &path, result);
            } else {
                result.push((
                    path.strip_prefix(root).unwrap().into(),
                    Some(fs::read(&path).unwrap()),
                ));
            }
        }
    }
    let mut result = Vec::new();
    walk(root, root, &mut result);
    result.sort();
    result
}

fn refuses(name: &str, bytes: &[u8], expected: &str) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let archive = root.join(name);
    let checksum = root.join("checksum");
    fs::write(&archive, bytes).unwrap();
    let existing = root.join("existing");
    fs::create_dir_all(existing.join("bin")).unwrap();
    fs::write(existing.join("bin/javelin"), b"existing binary\0\xff").unwrap();
    fs::create_dir(root.join("scratch")).unwrap();
    fs::write(root.join("scratch/keep"), b"scratch bytes\0\xfe").unwrap();
    let fresh = root.join("new-parent/install");
    let run = |dest: &Path| {
        Command::new(env!("CARGO_BIN_EXE_javelin-pack"))
            .args(["install", "--archive"])
            .arg(&archive)
            .arg("--checksum")
            .arg(&checksum)
            .arg("--dest")
            .arg(dest)
            .output()
            .unwrap()
    };
    // Checksum refusal must precede parsing, parent creation and destination checks.
    fs::write(&checksum, format!("{}  {name}\n", "0".repeat(64))).unwrap();
    let before = snapshot(root);
    for dest in [&fresh, &existing] {
        let output = run(dest);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("checksum mismatch"));
        assert_eq!(snapshot(root), before);
    }
    fs::write(&checksum, format!("{}  {name}\n", sha(bytes))).unwrap();
    let before = snapshot(root);
    let output = run(&fresh);
    assert!(!output.status.success(), "{name} unexpectedly installed");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains(expected), "{name}: {error}");
    assert_eq!(
        snapshot(root),
        before,
        "{name} changed destination/scratch bytes or directories"
    );
    let output = run(&existing);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already exists"));
    assert_eq!(snapshot(root), before);
}

#[test]
fn raw_zip_duplicate_is_rejected_before_deduplication() {
    let mut duplicate = zip(Some(("top/LASTONE", 1)));
    let archive = zip::ZipArchive::new(std::io::Cursor::new(&duplicate)).unwrap();
    let entry = archive.by_index_data(3).unwrap();
    let (local, central) = (
        entry.header_start() as usize,
        entry.central_header_start() as usize,
    );
    drop(archive);
    duplicate[local + 30..local + 41].copy_from_slice(b"top/LICENSE");
    duplicate[central + 46..central + 57].copy_from_slice(b"top/LICENSE");
    refuses("duplicate.zip", &duplicate, "duplicate");
}

#[test]
fn raw_zip_record_cap_is_rejected_before_deduplication() {
    let bytes = zip(None);
    let archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    let lic = archive.by_index_data(1).unwrap().central_header_start() as usize;
    let next = archive.by_index_data(2).unwrap().central_header_start() as usize;
    let eocd = bytes.len() - 22;
    let mut many = bytes[..eocd].to_vec();
    for _ in 0..10001 {
        many.extend_from_slice(&bytes[lic..next]);
    }
    let new_eocd = many.len();
    many.extend_from_slice(&bytes[eocd..]);
    many[new_eocd + 8..new_eocd + 10].copy_from_slice(&10004u16.to_le_bytes());
    many[new_eocd + 10..new_eocd + 12].copy_from_slice(&10004u16.to_le_bytes());
    let size = (new_eocd - archive.central_directory_start() as usize) as u32;
    many[new_eocd + 12..new_eocd + 16].copy_from_slice(&size.to_le_bytes());
    refuses("many.zip", &many, "more than 10000 raw entries");
}

#[test]
fn zip_directory_payload_is_rejected_without_skipping_expansion() {
    let mut bytes = zip(Some(("top/hidden/", 257 * 1024 * 1024)));
    set_zip_mode(&mut bytes, "top/hidden/", 0o040755, 3);
    assert!(bytes.len() < 4 * 1024 * 1024);
    refuses("directory.zip", &bytes, "carries a payload");
}

#[test]
fn pax_sparse_map_is_rejected_before_its_four_bytes_can_be_installed() {
    refuses(
        "sparse.tar.gz",
        &tar(true),
        "unsupported sparse tar extension",
    );
}

#[test]
fn zip_fifo_device_and_socket_modes_are_rejected() {
    for mode in [0o010644, 0o020644, 0o060644, 0o140644] {
        let mut bytes = zip(Some(("top/special", 4)));
        set_zip_mode(&mut bytes, "top/special", mode, 3);
        refuses("special.zip", &bytes, "unsupported type");
    }
}

#[test]
fn missing_gzip_trailer_is_rejected_before_destination_writes() {
    let bytes = tar(false);
    refuses(
        "missing.tar.gz",
        &bytes[..bytes.len() - 8],
        "unexpected end of file",
    );
}

#[test]
fn bad_gzip_crc_is_rejected_before_destination_writes() {
    let mut bad = tar(false);
    let trailer = bad.len() - 8;
    bad[trailer] ^= 1;
    refuses("crc.tar.gz", &bad, "corrupt gzip stream");
}

#[test]
fn clean_tar_and_dos_zip_without_unix_type_bits_still_install() {
    let mut dos = zip(None);
    for (name, _) in base_files() {
        set_zip_mode(&mut dos, &name, 0, 0);
    }
    for (name, bytes) in [("clean.tar.gz", tar(false)), ("dos.zip", dos)] {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join(name);
        let checksum = temp.path().join("checksum");
        let dest = temp.path().join("install");
        fs::write(&archive, &bytes).unwrap();
        fs::write(&checksum, format!("{}  {name}\n", sha(&bytes))).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_javelin-pack"))
            .args(["install", "--archive"])
            .arg(&archive)
            .arg("--checksum")
            .arg(&checksum)
            .arg("--dest")
            .arg(&dest)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        for (name, data) in base_files() {
            assert_eq!(
                fs::read(dest.join(name.strip_prefix("top/").unwrap())).unwrap(),
                data
            );
        }
    }
}

#[test]
fn zip_raw_record_cap_does_not_trust_a_false_footer_count() {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, data) in base_files() {
        writer.start_file(name, options).unwrap();
        writer.write_all(&data).unwrap();
    }
    for i in 0..9998 {
        writer.start_file(format!("top/empty{i}"), options).unwrap();
    }
    let mut bytes = writer.finish().unwrap().into_inner();
    let eocd = bytes.len() - 22;
    bytes[eocd + 8..eocd + 10].copy_from_slice(&3u16.to_le_bytes());
    bytes[eocd + 10..eocd + 12].copy_from_slice(&3u16.to_le_bytes());
    refuses("false-count.zip", &bytes, "more than 10000 raw entries");
}
