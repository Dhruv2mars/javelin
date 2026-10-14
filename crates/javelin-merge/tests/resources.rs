use javelin::{FileEntry, Manifest, ObjectId, RepoPath};
use javelin_merge::{MergeError, MergeOutcome, Merged, Resource, merge};

fn id(n: usize) -> ObjectId {
    ObjectId::parse(&format!("{n:064x}")).unwrap()
}
fn manifest(n: usize, paths: &[String]) -> Manifest {
    Manifest::new(paths.iter().map(|p| {
        (
            RepoPath::parse(p).unwrap(),
            FileEntry::Regular {
                content: id(n),
                executable: false,
            },
        )
    }))
    .unwrap()
}
fn run(texts: [&[u8]; 3], paths: &[String]) -> (Result<MergeOutcome, MergeError<()>>, usize) {
    let mut calls = 0;
    let result = merge(
        &manifest(0, paths),
        &manifest(1, paths),
        &manifest(2, paths),
        |object| {
            calls += 1;
            Ok(texts[(0..3).position(|n| id(n) == object).unwrap()].to_vec())
        },
    );
    (result, calls)
}
fn refused(result: Result<MergeOutcome, MergeError<()>>, expected: Resource) {
    let MergeError::Resource(limit) = result.unwrap_err() else {
        panic!("loader error")
    };
    assert_eq!(limit.resource, expected);
    assert!(limit.required > limit.limit);
    assert_eq!(limit.limit, expected.limit());
}

#[test]
fn oversized_blob_stops_before_next_load_and_binary_at_cap_is_semantic() {
    let cap = vec![0; Resource::BlobBytes.limit() as usize];
    let over = vec![0; cap.len() + 1];
    let paths = ["f".into()];
    let (at, calls) = run([&cap, b"o", b"t"], &paths);
    assert_eq!(calls, 3);
    assert!(!at.unwrap().is_clean());
    let (result, calls) = run([&over, b"o", b"t"], &paths);
    assert_eq!(calls, 1);
    refused(result, Resource::BlobBytes);
}

#[test]
fn aggregate_loader_bytes_are_not_reset_per_path() {
    let bytes = vec![0; Resource::BlobBytes.limit() as usize];
    let (result, calls) = run([&bytes; 3], &["a".into(), "b".into(), "c".into()]);
    assert_eq!(calls, 9);
    refused(result, Resource::LoadedBytes);
}

#[test]
fn aggregate_work_is_not_reset_per_path() {
    let bytes = [vec![b'a'; 149], b"\n".to_vec()].concat().repeat(500);
    // One path fits; three would reserve over the call-wide work limit.
    assert!(run([&bytes; 3], &["a".into()]).0.is_ok());
    let (result, calls) = run([&bytes; 3], &["a".into(), "b".into(), "c".into()]);
    refused(result, Resource::Work);
    assert_eq!(calls, 6, "do not load the third path after work refusal");
}

#[test]
fn deeply_nested_paths_and_long_common_prefixes_are_bounded() {
    let empty = Manifest::new([]).unwrap();
    let parent = manifest(0, &["a".into()]);
    let child_path = "a/".repeat(511) + "zz";
    assert_eq!(child_path.len(), Resource::PathBytes.limit() as usize);
    let child = manifest(1, &[child_path.clone()]);
    let no_load = |_| -> Result<Vec<u8>, ()> { panic!("metadata only") };
    let outcome = merge(&empty, &parent, &child, no_load).unwrap();
    assert_eq!(outcome.conflicts.len(), 2);
    assert!(outcome.merged.is_empty());
    assert_eq!(
        outcome.conflicts[0].with.as_ref().unwrap().as_str(),
        child_path
    );
    let over = manifest(1, &[child_path + "x"]);
    refused(merge(&empty, &parent, &over, no_load), Resource::PathBytes);
}

#[test]
fn caller_buffer_capacity_is_explicitly_outside_logical_byte_budget() {
    let path = ["f".into()];
    let outcome = merge(
        &manifest(0, &path),
        &manifest(1, &path),
        &manifest(2, &path),
        |_| {
            let mut bytes = Vec::with_capacity(2 * Resource::BlobBytes.limit() as usize);
            bytes.extend(b"a\n");
            Ok::<_, ()>(bytes)
        },
    )
    .unwrap();
    assert!(outcome.is_clean());
    assert_eq!(outcome.usage.loaded_bytes, 6);
}

#[test]
fn line_cap_includes_unterminated_final_line_before_table_allocation() {
    let cap = Resource::Lines.limit() as usize;
    for at in [
        b"a\n".repeat(cap),
        [b"a\n".repeat(cap - 1), b"a".to_vec()].concat(),
    ] {
        assert!(run([&at, b"o", b"t"], &["f".into()]).0.is_ok());
    }
    for over in [
        b"a\n".repeat(cap + 1),
        [b"a\n".repeat(cap), b"a".to_vec()].concat(),
    ] {
        refused(run([&over, b"o", b"t"], &["f".into()]).0, Resource::Lines);
    }
}

#[test]
fn table_cell_boundary_and_long_line_work_refuse_before_matching() {
    // (2047+1)^2 is exactly the cell cap; these repeated lines reach matching.
    let at = b"a\n".repeat(2047);
    assert!(run([&at; 3], &["f".into()]).0.is_ok());
    let over = b"a\n".repeat(2048);
    refused(run([&over; 3], &["f".into()]).0, Resource::TableCells);
    let long = [vec![b'a'; 511], b"\n".to_vec()].concat().repeat(1500);
    refused(run([&long; 3], &["f".into()]).0, Resource::Work);
}

#[test]
fn metadata_guards_run_before_any_loader_or_path_union() {
    let empty = Manifest::new([]).unwrap();
    let no_load = |_| -> Result<Vec<u8>, ()> { panic!("must refuse before loader") };
    let cap = Resource::Entries.limit() as usize;
    let paths: Vec<_> = (0..cap).map(|n| format!("file-{n:04}")).collect();
    let at = manifest(0, &paths);
    assert_eq!(
        merge(&empty, &at, &empty, no_load).unwrap().merged.len(),
        cap
    );
    let mut over = paths;
    over.push("over".into());
    refused(
        merge(&empty, &manifest(0, &over), &empty, no_load),
        Resource::Entries,
    );
    for len in [1024, 1025] {
        let one = manifest(0, &["a".repeat(len)]);
        let result = merge(&empty, &one, &empty, no_load);
        if len == 1024 {
            assert!(result.is_ok());
        } else {
            refused(result, Resource::PathBytes);
        }
    }
    let paths: Vec<_> = (0..4096).map(|n| format!("{n:032}")).collect();
    assert!(merge(&empty, &manifest(0, &paths), &empty, no_load).is_ok());
    let mut over = paths;
    over[0].push('a');
    refused(
        merge(&empty, &manifest(0, &over), &empty, no_load),
        Resource::TotalPathBytes,
    );
}

#[test]
fn adversarial_large_inputs_have_deterministic_typed_outcomes() {
    for n in [500, 2000, 4000, 8000] {
        let base: Vec<_> = (0..n).map(|i| format!("line-{i}\n")).collect();
        for shape in ["scattered", "moved", "repeated", "dissimilar"] {
            let (mut ours, mut theirs) = (base.clone(), base.clone());
            match shape {
                "scattered" => {
                    for i in (0..n).step_by(5) {
                        ours[i] = format!("ours-{i}\n");
                    }
                    for i in (2..n).step_by(5) {
                        theirs[i] = format!("theirs-{i}\n");
                    }
                }
                "moved" => {
                    ours.rotate_left(n / 2);
                    theirs[n - 1] = "tail\n".into();
                }
                "repeated" => {
                    ours.fill("}\n".into());
                    theirs.fill("\n".into());
                }
                _ => {
                    ours = (0..n).map(|i| format!("ours-{i}\n")).collect();
                    theirs = (0..n).map(|i| format!("theirs-{i}\n")).collect();
                }
            }
            let texts = [
                base.concat().into_bytes(),
                ours.concat().into_bytes(),
                theirs.concat().into_bytes(),
            ];
            let start = std::time::Instant::now();
            let result = run(texts.each_ref().map(Vec::as_slice), &["f".into()]).0;
            let elapsed = start.elapsed();
            assert_eq!(
                result,
                run(texts.each_ref().map(Vec::as_slice), &["f".into()]).0
            );
            if n <= 2000 {
                let outcome = result.unwrap();
                if shape == "scattered" {
                    assert!(outcome.is_clean());
                    let Merged::Content { bytes, .. } =
                        &outcome.merged[&RepoPath::parse("f").unwrap()]
                    else {
                        panic!()
                    };
                    let expected: String = (0..n)
                        .map(|i| {
                            if i % 5 == 0 {
                                &ours[i]
                            } else if i % 5 == 2 {
                                &theirs[i]
                            } else {
                                &base[i]
                            }
                        })
                        .cloned()
                        .collect();
                    assert_eq!(bytes, expected.as_bytes());
                }
                println!(
                    "{shape} lines={n} bytes={:?} work={} elapsed_ms={:.3} clean={}",
                    texts.each_ref().map(Vec::len),
                    outcome.usage.work_units,
                    elapsed.as_secs_f64() * 1000.,
                    outcome.is_clean()
                );
            } else {
                println!(
                    "{shape} lines={n} bytes={:?} elapsed_ms={:.3} refusal={result:?}",
                    texts.each_ref().map(Vec::len),
                    elapsed.as_secs_f64() * 1000.
                );
                refused(result, Resource::TableCells);
            }
        }
    }
}
