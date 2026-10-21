//! Behavior tests for the actual `javelin-pack` binary and the real `javelin` CLI it packages.
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::OnceLock,
};

use serde_json::Value;
use sha2::{Digest, Sha256};

const EPOCH: &str = "1781000000";

fn pack(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_javelin-pack"))
        .args(args)
        .output()
        .unwrap()
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn fail(output: &Output) -> String {
    assert!(!output.status.success(), "unexpected success: {output:?}");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn root() -> &'static str {
    static ROOT: OnceLock<String> = OnceLock::new();
    ROOT.get_or_init(|| source_root().to_str().unwrap().to_string())
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

struct Built {
    build_dir: PathBuf,
    archive: PathBuf,
    checksum: PathBuf,
    binary_sha: String,
}

/// Build the real CLI once with the production build command and package it.
fn built(label: &str, sha_char: char, rustflag: Option<&str>) -> Built {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("shared-{label}"));
    let (build_dir, dist) = (base.join("build"), base.join("dist"));
    let _ = fs::remove_dir_all(&dist);
    let mut args = vec![
        "build",
        "--source",
        root(),
        "--out",
        s(&build_dir),
        "--source-date-epoch",
        EPOCH,
    ];
    let flag;
    if let Some(f) = rustflag {
        flag = format!("--rustflag={f}");
        args.push(&flag);
    }
    ok(&pack(&args));
    let sha_arg = sha_char.to_string().repeat(40);
    let out = ok(&pack(&[
        "package",
        "--build",
        s(&build_dir),
        "--source",
        root(),
        "--source-sha",
        &sha_arg,
        "--out",
        s(&dist),
    ]));
    let value: Value = serde_json::from_str(&out).unwrap();
    let record: Value =
        serde_json::from_slice(&fs::read(build_dir.join("build-record.json")).unwrap()).unwrap();
    Built {
        build_dir,
        archive: value["archive"].as_str().unwrap().into(),
        checksum: value["checksum_file"].as_str().unwrap().into(),
        binary_sha: record["binary_sha256"].as_str().unwrap().into(),
    }
}

fn shared() -> &'static Built {
    static A: OnceLock<Built> = OnceLock::new();
    A.get_or_init(|| built("a", 'a', None))
}

fn install(b: &Built, dest: &Path) -> Output {
    pack(&[
        "install",
        "--archive",
        s(&b.archive),
        "--checksum",
        s(&b.checksum),
        "--dest",
        s(dest),
    ])
}

fn smoke(bin_dir: &Path) -> Output {
    pack(&["smoke", "--bin-dir", s(bin_dir)])
}

#[test]
fn package_installs_into_a_fresh_path_and_works_through_path() {
    let b = shared();
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("prefix").join("javelin");
    let installed: Value = serde_json::from_str(&ok(&install(b, &dest))).unwrap();
    assert_eq!(installed["binary_sha256"], b.binary_sha);

    let manifest: Value =
        serde_json::from_slice(&fs::read(dest.join("PACKAGE.json")).unwrap()).unwrap();
    assert_eq!(manifest["channel"], "development");
    assert_eq!(manifest["pre_ga"], true);
    assert_eq!(manifest["cargo_version"], "0.1.0");
    assert_eq!(manifest["source"]["sha"], "a".repeat(40));
    assert_eq!(manifest["platform"]["minimum_os_version"], "unconfirmed");
    assert!(
        manifest["toolchain"]["rustc_verbose"]
            .as_str()
            .unwrap()
            .contains("host: ")
    );
    assert!(manifest["toolchain"]["rustflags"].as_str().is_none());
    let deps = manifest["dependencies"].as_array().unwrap();
    assert!(
        deps.iter()
            .any(|d| d["name"] == "rusqlite" && d["checksum"].as_str().is_some())
    );
    assert!(
        deps.iter()
            .any(|d| d["name"] == "javelin" && d["source"].is_null())
    );
    assert!(!deps.iter().any(|d| d["name"] == "tempfile"));
    // Every listed dependency ships its license text.
    for d in deps {
        let dir = dest.join("licenses").join(format!(
            "{}-{}",
            d["name"].as_str().unwrap(),
            d["version"].as_str().unwrap()
        ));
        assert!(
            fs::read_dir(&dir).unwrap().next().is_some(),
            "{dir:?} has no license file"
        );
    }
    // The shipped LICENSE is the repository's actual file, byte for byte.
    assert_eq!(
        fs::read(dest.join("LICENSE")).unwrap(),
        fs::read(source_root().join("LICENSE")).unwrap()
    );
    let notices = fs::read_to_string(dest.join("THIRD-PARTY-NOTICES.md")).unwrap();
    assert!(notices.contains("libsqlite3-sys") && notices.contains("public domain"));
    assert!(
        fs::read_to_string(dest.join("README.txt"))
            .unwrap()
            .contains("not a release"),
        "README must say it is not a release"
    );

    let report: Value = serde_json::from_str(&ok(&smoke(&dest.join("bin")))).unwrap();
    assert_eq!(report["ok"], true);
    assert!(report["checks"].as_array().unwrap().len() >= 15);
}

#[test]
fn release_binary_has_no_test_hooks_and_packager_refuses_one_with_hooks() {
    let b = shared();
    let bytes = fs::read(b.build_dir.join("target/release").join(if cfg!(windows) {
        "javelin.exe"
    } else {
        "javelin"
    }))
    .unwrap();
    for marker in ["JAVELIN_TEST_", "process_tests"] {
        assert!(
            !bytes.windows(marker.len()).any(|w| w == marker.as_bytes()),
            "release binary contains {marker}"
        );
    }
    // Same record, binary with a test-only marker appended: refused.
    let tampered = tempfile::tempdir().unwrap();
    let rel = "target/release";
    fs::create_dir_all(tampered.path().join(rel)).unwrap();
    let name = if cfg!(windows) {
        "javelin.exe"
    } else {
        "javelin"
    };
    let mut with_hook = bytes.clone();
    with_hook.extend_from_slice(b"JAVELIN_TEST_ROOT");
    fs::write(tampered.path().join(rel).join(name), &with_hook).unwrap();
    let mut record: Value =
        serde_json::from_slice(&fs::read(b.build_dir.join("build-record.json")).unwrap()).unwrap();
    record["binary_sha256"] = sha(&with_hook).into();
    fs::write(
        tampered.path().join("build-record.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    let err = fail(&pack(&[
        "package",
        "--build",
        s(tampered.path()),
        "--source",
        root(),
        "--source-sha",
        &"c".repeat(40),
        "--out",
        s(out.path()),
    ]));
    assert!(err.contains("test-only marker"), "{err}");
    assert!(
        fs::read_dir(out.path()).unwrap().next().is_none(),
        "no archive may be written"
    );

    // A binary that does not match its build record is refused.
    fs::write(tampered.path().join(rel).join(name), &bytes).unwrap();
    let mut record2 = record.clone();
    record2["binary_sha256"] = sha(b"other").into();
    fs::write(
        tampered.path().join("build-record.json"),
        serde_json::to_vec(&record2).unwrap(),
    )
    .unwrap();
    let err = fail(&pack(&[
        "package",
        "--build",
        s(tampered.path()),
        "--source",
        root(),
        "--source-sha",
        &"c".repeat(40),
        "--out",
        s(out.path()),
    ]));
    assert!(err.contains("does not match its build record"), "{err}");

    // An executable that is not the Javelin CLI is refused.
    let impostor =
        fs::read(std::env::var_os("CARGO").expect("cargo sets CARGO for tests")).unwrap();
    fs::write(tampered.path().join(rel).join(name), &impostor).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            tampered.path().join(rel).join(name),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    record2["binary_sha256"] = sha(&impostor).into();
    fs::write(
        tampered.path().join("build-record.json"),
        serde_json::to_vec(&record2).unwrap(),
    )
    .unwrap();
    let err = fail(&pack(&[
        "package",
        "--build",
        s(tampered.path()),
        "--source",
        root(),
        "--source-sha",
        &"c".repeat(40),
        "--out",
        s(out.path()),
    ]));
    assert!(err.contains("--help") || err.contains("exited"), "{err}");
    assert!(
        fs::read_dir(out.path()).unwrap().next().is_none(),
        "no archive may be written"
    );
}

#[test]
fn source_sha_must_be_forty_lowercase_hex() {
    let b = shared();
    let out = tempfile::tempdir().unwrap();
    for bad in ["abc", &"A".repeat(40), &"g".repeat(40)] {
        let err = fail(&pack(&[
            "package",
            "--build",
            s(&b.build_dir),
            "--source",
            root(),
            "--source-sha",
            bad,
            "--out",
            s(out.path()),
        ]));
        assert!(err.contains("40 lowercase hex"), "{err}");
    }
}

#[test]
fn archive_bytes_are_deterministic_for_the_same_binary_and_epoch() {
    let b = shared();
    let again = tempfile::tempdir().unwrap();
    let value: Value = serde_json::from_str(&ok(&pack(&[
        "package",
        "--build",
        s(&b.build_dir),
        "--source",
        root(),
        "--source-sha",
        &"a".repeat(40),
        "--out",
        s(again.path()),
    ])))
    .unwrap();
    assert_eq!(
        fs::read(value["archive"].as_str().unwrap()).unwrap(),
        fs::read(&b.archive).unwrap()
    );
    // Entry metadata is normalized: build-record epoch, root ownership, fixed modes.
    let mut count = 0;
    if b.archive.extension().unwrap() == "zip" {
        let mut archive = zip::ZipArchive::new(fs::File::open(&b.archive).unwrap()).unwrap();
        for index in 0..archive.len() {
            let file = archive.by_index(index).unwrap();
            let t = file.last_modified().unwrap();
            // 1781000000 is 2026-06-09 10:13:20 UTC.
            assert_eq!(
                (
                    t.year(),
                    t.month(),
                    t.day(),
                    t.hour(),
                    t.minute(),
                    t.second()
                ),
                (2026, 6, 9, 10, 13, 20)
            );
            // zip records the writing host as DOS on Windows, so unix modes are not
            // meaningful there (and executable bits do not exist on Windows).
            if !cfg!(windows) {
                let mode = file.unix_mode().unwrap() & 0o777;
                assert!(matches!(mode, 0o755 | 0o644), "{mode:o}");
            }
            count += 1;
        }
    } else {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(
            fs::File::open(&b.archive).unwrap(),
        ));
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let header = entry.header();
            assert_eq!(header.mtime().unwrap(), EPOCH.parse::<u64>().unwrap());
            assert_eq!((header.uid().unwrap(), header.gid().unwrap()), (0, 0));
            let mode = header.mode().unwrap();
            assert!(matches!(mode, 0o755 | 0o644), "{mode:o}");
            count += 1;
        }
    }
    assert!(count > 40, "{count} entries");
}

#[test]
fn tampered_archive_or_checksum_is_rejected_and_leaves_nothing() {
    let b = shared();
    let temp = tempfile::tempdir().unwrap();
    let mut bytes = fs::read(&b.archive).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x55;
    let name = b.archive.file_name().unwrap();
    let archive = temp.path().join(name);
    fs::write(&archive, &bytes).unwrap();
    let dest = temp.path().join("prefix/javelin");
    let err = fail(&pack(&[
        "install",
        "--archive",
        s(&archive),
        "--checksum",
        s(&b.checksum),
        "--dest",
        s(&dest),
    ]));
    assert!(err.contains("checksum mismatch"), "{err}");
    assert!(!dest.exists() && !temp.path().join("prefix").exists());

    // Checksum file for a different file name, and malformed checksum text.
    let wrong = temp.path().join("wrong.sha256");
    fs::write(&wrong, format!("{}  other.tar.gz\n", sha(&bytes))).unwrap();
    assert!(
        fail(&pack(&[
            "install",
            "--archive",
            s(&archive),
            "--checksum",
            s(&wrong),
            "--dest",
            s(&dest)
        ]))
        .contains("names")
    );
    fs::write(&wrong, "not-a-checksum\n").unwrap();
    assert!(
        fail(&pack(&[
            "install",
            "--archive",
            s(&archive),
            "--checksum",
            s(&wrong),
            "--dest",
            s(&dest)
        ]))
        .contains("checksum file")
    );
    assert!(!dest.exists());

    // Correct checksum for a truncated archive: extraction fails and owned temp is removed.
    let truncated = temp.path().join("cut").join(name);
    fs::create_dir_all(truncated.parent().unwrap()).unwrap();
    let original = fs::read(&b.archive).unwrap();
    fs::write(&truncated, &original[..original.len() / 2]).unwrap();
    let sum = temp.path().join("cut.sha256");
    fs::write(
        &sum,
        format!(
            "{}  {}\n",
            sha(&original[..original.len() / 2]),
            name.to_string_lossy()
        ),
    )
    .unwrap();
    fail(&pack(&[
        "install",
        "--archive",
        s(&truncated),
        "--checksum",
        s(&sum),
        "--dest",
        s(&dest),
    ]));
    assert!(!dest.exists());
    assert!(
        !temp.path().join("prefix").exists(),
        "truncated archive created a destination parent"
    );
}

#[test]
fn existing_destination_is_never_touched() {
    let b = shared();
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("javelin");
    fs::create_dir_all(dest.join("bin")).unwrap();
    let preexisting = [1u8, 2, 3, 0, 255];
    fs::write(
        dest.join("bin").join(if cfg!(windows) {
            "javelin.exe"
        } else {
            "javelin"
        }),
        preexisting,
    )
    .unwrap();
    assert!(fail(&install(b, &dest)).contains("already exists"));
    assert_eq!(
        fs::read(dest.join("bin").join(if cfg!(windows) {
            "javelin.exe"
        } else {
            "javelin"
        }))
        .unwrap(),
        preexisting
    );
    assert_eq!(
        fs::read_dir(temp.path()).unwrap().count(),
        1,
        "no temp residue beside an existing destination"
    );

    let file_dest = temp.path().join("file");
    fs::write(&file_dest, b"keep").unwrap();
    assert!(fail(&install(b, &file_dest)).contains("already exists"));
    assert_eq!(fs::read(&file_dest).unwrap(), b"keep");
}

type TarEntry<'a> = (&'a str, tar::EntryType, &'a str);
type ZipBuf = zip::ZipWriter<std::io::Cursor<Vec<u8>>>;

fn raw_tar(entries: &[TarEntry]) -> Vec<u8> {
    let mut builder =
        tar::Builder::new(flate2::GzBuilder::new().write(Vec::new(), flate2::Compression::fast()));
    for (name, kind, link) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(*kind);
        header.set_mode(0o755);
        header.set_size(if *kind == tar::EntryType::Regular {
            4
        } else {
            0
        });
        header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
        if !link.is_empty() {
            header.set_link_name(link).unwrap();
        }
        header.set_cksum();
        let body: &[u8] = if *kind == tar::EntryType::Regular {
            b"data"
        } else {
            b""
        };
        builder.append(&header, body).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn try_install_bytes(file_name: &str, bytes: &[u8]) -> (String, PathBuf, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("in").join(file_name);
    fs::create_dir_all(archive.parent().unwrap()).unwrap();
    fs::write(&archive, bytes).unwrap();
    let sum = temp.path().join("in").join(format!("{file_name}.sha256"));
    fs::write(&sum, format!("{}  {file_name}\n", sha(bytes))).unwrap();
    let dest = temp.path().join("prefix").join("javelin");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let err = fail(&pack(&[
        "install",
        "--archive",
        s(&archive),
        "--checksum",
        s(&sum),
        "--dest",
        s(&dest),
    ]));
    (err, dest, temp)
}

#[test]
fn unsafe_tar_entries_are_rejected_even_with_a_valid_checksum() {
    use tar::EntryType::{Directory, Link, Regular, Symlink};
    let cases: &[(&str, &[TarEntry], &str)] = &[
        (
            "parent traversal",
            &[("top/", Directory, ""), ("top/../../evil", Regular, "")],
            "unsafe component",
        ),
        (
            "leading parent",
            &[("../evil", Regular, "")],
            "unsafe component",
        ),
        (
            "absolute path",
            &[("/etc/evil", Regular, "")],
            "unsafe component",
        ),
        (
            "backslash",
            &[("top\\evil", Regular, "")],
            "unsafe component",
        ),
        (
            "drive letter",
            &[("C:/evil", Regular, "")],
            "unsafe component",
        ),
        (
            "reserved device",
            &[("top/CON", Regular, "")],
            "unsafe component",
        ),
        (
            "symlink",
            &[
                ("top/", Directory, ""),
                ("top/link", Symlink, "/etc/passwd"),
            ],
            "unsupported type",
        ),
        (
            "hardlink",
            &[("top/", Directory, ""), ("top/hard", Link, "top/other")],
            "unsupported type",
        ),
        (
            "duplicate",
            &[("top/a", Regular, ""), ("top/a", Regular, "")],
            "duplicate",
        ),
        (
            "case duplicate",
            &[("top/a", Regular, ""), ("top/A", Regular, "")],
            "duplicate",
        ),
        (
            "file and directory",
            &[("top/a", Regular, ""), ("top/a/b", Regular, "")],
            "both a file and a directory",
        ),
        (
            "two top directories",
            &[("one/a", Regular, ""), ("two/b", Regular, "")],
            "single top-level",
        ),
    ];
    for (label, entries, expect) in cases {
        let (err, dest, temp) = try_install_bytes("evil.tar.gz", &raw_tar(entries));
        assert!(err.contains(expect), "{label}: {err}");
        assert!(!dest.exists(), "{label}: destination created");
        assert_eq!(
            fs::read_dir(temp.path().join("prefix")).unwrap().count(),
            0,
            "{label}: residue"
        );
        assert!(
            !temp.path().join("evil").exists()
                && !temp.path().parent().unwrap().join("evil").exists(),
            "{label}: escaped"
        );
    }
}

#[test]
fn unsafe_zip_entries_are_rejected_even_with_a_valid_checksum() {
    let make = |build: &dyn Fn(&mut ZipBuf)| {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        build(&mut writer);
        writer.finish().unwrap().into_inner()
    };
    let options = zip::write::SimpleFileOptions::default();
    let traversal = make(&|w| {
        w.start_file("top/../../evil", options).unwrap();
        w.write_all(b"x").unwrap();
    });
    let (err, dest, _t) = try_install_bytes("evil.zip", &traversal);
    assert!(err.contains("unsafe component"), "{err}");
    assert!(!dest.exists());
    let backslash = make(&|w| {
        w.start_file("top\\..\\evil", options).unwrap();
        w.write_all(b"x").unwrap();
    });
    assert!(
        try_install_bytes("evil.zip", &backslash)
            .0
            .contains("unsafe component")
    );
    let symlink = make(&|w| {
        w.add_symlink("top/link", "/etc/passwd", options).unwrap();
    });
    let (err, dest, _t) = try_install_bytes("evil.zip", &symlink);
    assert!(err.contains("symlink"), "{err}");
    assert!(!dest.exists());
    assert!(
        try_install_bytes("evil.rar", b"x")
            .0
            .contains("unsupported archive type")
    );
}

#[test]
fn zip_package_installs_with_the_same_guarantees_on_every_os() {
    // Re-wrap the real package as a zip, the format Windows packages use.
    let b = shared();
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("from-tar");
    ok(&install(b, &dest));
    let top = b
        .archive
        .file_name()
        .unwrap()
        .to_string_lossy()
        .trim_end_matches(".tar.gz")
        .to_string();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    writer.add_directory(format!("{top}/"), options).unwrap();
    for file in walk(&dest) {
        let rel = file
            .strip_prefix(&dest)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let exec = rel.starts_with("bin/");
        writer
            .start_file(
                format!("{top}/{rel}"),
                options.unix_permissions(if exec { 0o755 } else { 0o644 }),
            )
            .unwrap();
        writer.write_all(&fs::read(&file).unwrap()).unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();
    let zip_path = temp.path().join(format!("{top}.zip"));
    fs::write(&zip_path, &bytes).unwrap();
    let sum = temp.path().join(format!("{top}.zip.sha256"));
    fs::write(&sum, format!("{}  {top}.zip\n", sha(&bytes))).unwrap();
    let zip_dest = temp.path().join("from-zip");
    ok(&pack(&[
        "install",
        "--archive",
        s(&zip_path),
        "--checksum",
        s(&sum),
        "--dest",
        s(&zip_dest),
    ]));
    let report: Value = serde_json::from_str(&ok(&smoke(&zip_dest.join("bin")))).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["binary_sha256"], b.binary_sha);
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out.sort();
    out
}

#[test]
fn smoke_rejects_a_binary_that_is_not_the_javelin_cli() {
    // The packager itself, renamed, is a real executable but not the CLI.
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_javelin-pack"),
        bin.join(if cfg!(windows) {
            "javelin.exe"
        } else {
            "javelin"
        }),
    )
    .unwrap();
    let output = smoke(&bin);
    let stderr = fail(&output);
    assert!(stderr.contains("smoke failed"), "{stderr}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
}

#[cfg(unix)]
#[test]
fn smoke_detects_a_binary_that_invokes_git() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let script = bin.join("javelin");
    fs::write(
        &script,
        "#!/bin/sh\ncase \"$1\" in --help) echo 'Javelin native repository foundation'; exit 0;; esac\ngit rev-parse --git-dir >/dev/null 2>&1\necho '{}'\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let output = smoke(&bin);
    fail(&output);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let git = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Git was never invoked")
        .unwrap();
    assert_eq!(git["ok"], false);
}

#[test]
fn upgrade_and_rollback_are_side_by_side_installs_of_the_development_binary() {
    let a = shared();
    let b = built("b", 'b', Some("-Copt-level=2"));
    assert_ne!(
        a.binary_sha, b.binary_sha,
        "second build must be a genuinely different binary"
    );
    let temp = tempfile::tempdir().unwrap();
    let (old, new) = (
        temp.path().join("versions/old"),
        temp.path().join("versions/new"),
    );
    ok(&install(a, &old));
    let old_bin = old.join("bin").join(if cfg!(windows) {
        "javelin.exe"
    } else {
        "javelin"
    });
    let old_bytes = fs::read(&old_bin).unwrap();
    ok(&install(&b, &new));
    let new_manifest: Value =
        serde_json::from_slice(&fs::read(new.join("PACKAGE.json")).unwrap()).unwrap();
    assert_eq!(new_manifest["source"]["sha"], "b".repeat(40));
    assert_eq!(
        fs::read(&old_bin).unwrap(),
        old_bytes,
        "install of the newer package changed the older one"
    );

    // Upgrade: PATH selects the new install; both pass the full smoke.
    assert_eq!(
        serde_json::from_str::<Value>(&ok(&smoke(&new.join("bin")))).unwrap()["binary_sha256"],
        b.binary_sha
    );
    // A corrupt newer package fails without disturbing the working one.
    let mut bytes = fs::read(&b.archive).unwrap();
    bytes[100] ^= 1;
    let bad = temp.path().join(b.archive.file_name().unwrap());
    fs::write(&bad, &bytes).unwrap();
    let failed = temp.path().join("versions/failed");
    assert!(
        fail(&pack(&[
            "install",
            "--archive",
            s(&bad),
            "--checksum",
            s(&b.checksum),
            "--dest",
            s(&failed)
        ]))
        .contains("checksum mismatch")
    );
    assert!(!failed.exists());
    // Rollback: the previous install is byte-identical and still passes.
    assert_eq!(fs::read(&old_bin).unwrap(), old_bytes);
    assert_eq!(
        serde_json::from_str::<Value>(&ok(&smoke(&old.join("bin")))).unwrap()["binary_sha256"],
        a.binary_sha
    );
}

fn bomb(kind: tar::EntryType, name: &str, size: u64) -> Vec<u8> {
    let mut builder =
        tar::Builder::new(flate2::GzBuilder::new().write(Vec::new(), flate2::Compression::fast()));
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(kind);
    header.set_mode(0o644);
    header.set_size(size);
    header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
    header.set_cksum();
    builder
        .append(&header, std::io::repeat(0).take(size))
        .unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

#[test]
fn oversized_expansion_is_rejected_for_files_and_directory_payloads() {
    let over = 257 * 1024 * 1024;
    let file = bomb(tar::EntryType::Regular, "top/big", over);
    assert!(file.len() < 4 * 1024 * 1024, "fixture should compress well");
    let (err, dest, temp) = try_install_bytes("bomb.tar.gz", &file);
    assert!(
        err.contains("expands past") || err.contains("unexpected EOF"),
        "{err}"
    );
    assert!(!dest.exists());
    assert_eq!(fs::read_dir(temp.path().join("prefix")).unwrap().count(), 0);
    // The cap also covers tar metadata, which the parser buffers before any entry is read.
    let mut builder =
        tar::Builder::new(flate2::GzBuilder::new().write(Vec::new(), flate2::Compression::fast()));
    let comment = vec![b'x'; 280 * 1024 * 1024];
    builder
        .append_pax_extensions([("comment", comment.as_slice())])
        .unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    builder
        .append_data(&mut header, "top/small", std::io::empty())
        .unwrap();
    let pax = builder.into_inner().unwrap().finish().unwrap();
    let (err, dest, _t) = try_install_bytes("pax.tar.gz", &pax);
    assert!(
        !err.contains("PACKAGE.json") && !dest.exists(),
        "metadata flood must fail during extraction: {err}"
    );
    // A directory header that smuggles a payload is refused outright.
    let dir = bomb(tar::EntryType::Directory, "top/", 1024 * 1024);
    let (err, dest, _t) = try_install_bytes("bomb.tar.gz", &dir);
    assert!(err.contains("carries a payload"), "{err}");
    assert!(!dest.exists());
}

#[test]
fn valid_looking_package_with_a_wrong_binary_hash_leaves_no_temp_directory() {
    let b = shared();
    let temp = tempfile::tempdir().unwrap();
    let good = temp.path().join("good");
    ok(&install(b, &good));
    let top = b
        .archive
        .file_name()
        .unwrap()
        .to_string_lossy()
        .trim_end_matches(".tar.gz")
        .trim_end_matches(".zip")
        .to_string();
    let mut builder =
        tar::Builder::new(flate2::GzBuilder::new().write(Vec::new(), flate2::Compression::fast()));
    for file in walk(&good) {
        let rel = file
            .strip_prefix(&good)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let mut data = fs::read(&file).unwrap();
        if rel.starts_with("bin/") {
            *data.last_mut().unwrap() ^= 1;
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_uid(0);
        header.set_gid(0);
        builder
            .append_data(&mut header, format!("{top}/{rel}"), data.as_slice())
            .unwrap();
    }
    let bytes = builder.into_inner().unwrap().finish().unwrap();
    let (err, dest, temp2) = try_install_bytes("tampered-inside.tar.gz", &bytes);
    assert!(err.contains("does not match manifest"), "{err}");
    assert!(!dest.exists());
    assert_eq!(
        fs::read_dir(temp2.path().join("prefix")).unwrap().count(),
        0,
        "owned temp directory left behind"
    );
}

fn copy_build_sources(dest: &Path) {
    let root_dir = source_root();
    for file in ["Cargo.toml", "Cargo.lock", "LICENSE"] {
        fs::copy(root_dir.join(file), dest.join(file)).unwrap();
    }
    for file in walk(&root_dir.join("src"))
        .into_iter()
        .chain(walk(&root_dir.join("crates/javelin-cli")))
        .chain(walk(&root_dir.join("crates/javelin-collaboration")))
        .chain(walk(&root_dir.join("crates/javelin-identity")))
        .chain(walk(&root_dir.join("crates/javelin-merge")))
        .chain(walk(&root_dir.join("crates/javelin-node")))
        .chain(walk(&root_dir.join("crates/javelin-protocol")))
        .chain(walk(&root_dir.join("crates/javelin-stack")))
    {
        let rel = file.strip_prefix(&root_dir).unwrap();
        let target = dest.join(rel);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(&file, target).unwrap();
    }
}

// Wrap real Cargo to make a save after compilation deterministic, without timing
// a background writer or changing the developer checkout.
#[cfg(unix)]
#[test]
fn build_refuses_sources_changed_during_cargo_and_removes_stale_record() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    copy_build_sources(&source);
    let wrappers = temp.path().join("wrappers");
    fs::create_dir(&wrappers).unwrap();
    let wrapper = wrappers.join("cargo");
    fs::write(
        &wrapper,
        "#!/bin/sh\nset -eu\n\"$JAVELIN_TEST_REAL_CARGO\" \"$@\"\nif [ \"$1\" = build ]; then\n  printf '\\n// saved during build\\n' >> \"$JAVELIN_TEST_EDIT\"\nfi\n",
    ).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let out = temp.path().join("build");
    fs::create_dir(&out).unwrap();
    fs::write(out.join("build-record.json"), b"stale record").unwrap();
    let mut paths = vec![wrappers];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let output = Command::new(env!("CARGO_BIN_EXE_javelin-pack"))
        .args([
            "build",
            "--source",
            s(&source),
            "--out",
            s(&out),
            "--source-date-epoch",
            EPOCH,
        ])
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env(
            "JAVELIN_TEST_REAL_CARGO",
            std::env::var_os("CARGO").unwrap(),
        )
        .env(
            "JAVELIN_TEST_EDIT",
            source.join("crates/javelin-cli/src/main.rs"),
        )
        .output()
        .unwrap();
    assert!(fail(&output).contains("source changed during cargo build"));
    assert!(
        out.join("target/release/javelin").is_file(),
        "real Cargo must have built the CLI"
    );
    assert!(!out.join("build-record.json").exists());
}

#[test]
fn package_rejects_sources_that_differ_from_what_was_built() {
    let b = shared();
    let copy = tempfile::tempdir().unwrap();
    copy_build_sources(copy.path());
    let out = tempfile::tempdir().unwrap();
    let args = |source: &Path| {
        pack(&[
            "package",
            "--build",
            s(&b.build_dir),
            "--source",
            s(source),
            "--source-sha",
            &"d".repeat(40),
            "--out",
            s(out.path()),
        ])
    };
    ok(&args(copy.path()));
    for input in [
        "crates/javelin-cli/Cargo.toml",
        "crates/javelin-cli/src/main.rs",
        "crates/javelin-cli/src/cli.rs",
        "crates/javelin-cli/src/cli_workspace.rs",
        "crates/javelin-merge/Cargo.toml",
        "crates/javelin-merge/LICENSE",
        "crates/javelin-merge/src/lib.rs",
        "crates/javelin-merge/src/matching.rs",
        "crates/javelin-merge/src/resources.rs",
        "crates/javelin-stack/Cargo.toml",
        "crates/javelin-stack/LICENSE",
        "crates/javelin-node/Cargo.toml",
        "crates/javelin-node/src/lib.rs",
        "crates/javelin-protocol/Cargo.toml",
        "crates/javelin-protocol/src/lib.rs",
        "crates/javelin-collaboration/Cargo.toml",
        "crates/javelin-collaboration/src/lib.rs",
        "crates/javelin-stack/src/lib.rs",
        "src/lib.rs",
    ] {
        let path = copy.path().join(input);
        let original = fs::read(&path).unwrap();
        let mut edited = original.clone();
        edited.extend_from_slice(if input.ends_with(".toml") {
            b"\n# edited after the build\n"
        } else {
            b"\n// edited after the build\n"
        });
        fs::write(&path, edited).unwrap();
        assert!(
            fail(&args(copy.path())).contains("does not match the sources"),
            "unfingerprinted input: {input}"
        );
        fs::write(path, original).unwrap();
    }
}

#[test]
fn build_works_when_paths_contain_spaces_and_quotes() {
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("build with 'quote' and spaces");
    ok(&pack(&[
        "build",
        "--source",
        root(),
        "--out",
        s(&out),
        "--source-date-epoch",
        EPOCH,
    ]));
    let record: Value =
        serde_json::from_slice(&fs::read(out.join("build-record.json")).unwrap()).unwrap();
    assert_eq!(record["binary_sha256"].as_str().unwrap().len(), 64);
}

#[test]
fn stream_cut_exactly_on_a_header_boundary_is_not_mistaken_for_a_clean_end() {
    // Layout (bytes): [pax header 512][pax data T][file header 512][file data F][symlink header ...]
    // with the symlink header starting exactly at the decompressed-stream cap (272 MiB).
    let cap: u64 = 272 * 1024 * 1024;
    let pax_data: u64 = 40 * 1024 * 1024;
    let file: u64 = cap - 1024 - pax_data;
    let mut builder =
        tar::Builder::new(flate2::GzBuilder::new().write(Vec::new(), flate2::Compression::fast()));
    // Record is "<len> comment=<value>\n" where len (8 digits) counts the whole record.
    let value = vec![b'x'; (pax_data - 18) as usize];
    builder
        .append_pax_extensions([("comment", value.as_slice())])
        .unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_size(file);
    header.set_mode(0o644);
    builder
        .append_data(&mut header, "top/filler", std::io::repeat(0).take(file))
        .unwrap();
    let mut link = tar::Header::new_gnu();
    link.set_entry_type(tar::EntryType::Symlink);
    link.set_size(0);
    builder
        .append_link(&mut link, "top/link", "/etc/passwd")
        .unwrap();
    let bytes = builder.into_inner().unwrap().finish().unwrap();
    let (err, dest, temp) = try_install_bytes("boundary.tar.gz", &bytes);
    assert!(
        !err.contains("PACKAGE.json") && !err.contains("unsupported type"),
        "must fail on the cap, not later: {err}"
    );
    assert!(err.contains("decompressed stream cap"), "{err}");
    assert!(!dest.exists());
    assert_eq!(fs::read_dir(temp.path().join("prefix")).unwrap().count(), 0);
}

#[test]
fn repro_builds_two_directories_and_reports_honestly() {
    let temp = tempfile::tempdir().unwrap();
    let report_path = temp.path().join("report.json");
    ok(&pack(&[
        "repro",
        "--source",
        root(),
        "--work",
        s(&temp.path().join("work")),
        "--report",
        s(&report_path),
        "--source-date-epoch",
        EPOCH,
    ]));
    let report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    let (a, b) = (&report["a"], &report["b"]);
    assert_ne!(
        a["source_dir"], b["source_dir"],
        "builds must come from different directories"
    );
    // The reported verdict must agree with the hashes it reports; it may be either value.
    assert_eq!(
        report["identical"].as_bool().unwrap(),
        a["binary_sha256"] == b["binary_sha256"]
    );
    if report["identical"] == false {
        assert!(
            !report["note"]
                .as_str()
                .unwrap()
                .contains("identical binary bytes")
        );
    }
}
