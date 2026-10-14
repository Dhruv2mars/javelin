//! Records the encoded size of version 1 binary messages against the rejected
//! JSON-with-base64 candidate for the same fixture. Run with `--nocapture`.

mod support;

use javelin::{FileEntry, Manifest, RepoPath};
use javelin_protocol::{Message, codec::encode};
use support::repo;

fn base64_len(n: usize) -> usize {
    n.div_ceil(3) * 4
}

/// Byte length of the candidate A body: {"v":1,"t":"blobs","objects":[{"id":..,"data":..}]}.
fn json_blobs(blobs: &[(javelin::ObjectId, Vec<u8>)]) -> usize {
    let objects: Vec<String> = blobs
        .iter()
        .map(|(id, b)| {
            format!(
                r#"{{"id":"{id}","data":"{}"}}"#,
                "A".repeat(base64_len(b.len()))
            )
        })
        .collect();
    format!(r#"{{"v":1,"t":"blobs","objects":[{}]}}"#, objects.join(",")).len()
}

/// Candidate A manifest body reuses the native public FileEntry JSON shape.
fn json_manifest(id: javelin::ObjectId, manifest: &Manifest) -> usize {
    let entries: Vec<String> = manifest
        .entries()
        .iter()
        .map(|(path, e)| match e {
            FileEntry::Regular { content, executable } => format!(
                r#"{{"path":"{}","entry":{{"kind":"regular","content":"{content}","executable":{executable}}}}}"#,
                path.as_str()
            ),
            FileEntry::Symlink { target } => format!(
                r#"{{"path":"{}","entry":{{"kind":"symlink","target":"{target}"}}}}"#,
                path.as_str()
            ),
        })
        .collect();
    format!(
        r#"{{"v":1,"t":"manifests","objects":[{{"id":"{id}","entries":[{}]}}]}}"#,
        entries.join(",")
    )
    .len()
}

#[test]
fn binary_is_smaller_than_json_for_the_fixture() {
    let (_d, mut source) = repo();
    let mut blobs = Vec::new();
    let mut entries = Vec::new();
    for i in 0..200u32 {
        let bytes: Vec<u8> = (0..2048u32)
            .map(|j| (i.wrapping_mul(31) ^ j.wrapping_mul(17)) as u8)
            .collect();
        let id = source.put_blob(&bytes).unwrap();
        blobs.push((id, bytes));
        let path = RepoPath::parse(&format!("src/module_{}/file_{i}.rs", i % 20)).unwrap();
        entries.push((
            path,
            FileEntry::Regular {
                content: id,
                executable: i % 50 == 0,
            },
        ));
    }
    let manifest = Manifest::new(entries).unwrap();
    let id = source.put_manifest(&manifest).unwrap();
    let raw: usize = blobs.iter().map(|(_, b)| b.len()).sum();
    let binary_blobs = encode(&Message::Blobs(blobs.clone())).len();
    let binary_manifest = encode(&Message::Manifests(vec![(id, manifest.clone())])).len();
    let (json_b, json_m) = (json_blobs(&blobs), json_manifest(id, &manifest));
    println!("fixture: 200 blobs x 2048 bytes = {raw} raw bytes, 200 manifest entries");
    println!("blobs: binary {binary_blobs} json {json_b}");
    println!(
        "manifest: binary {binary_manifest} json {json_m} canonical {}",
        manifest.canonical_bytes().len()
    );
    assert!(binary_blobs < json_b && binary_manifest < json_m);
    assert_eq!(binary_blobs, 1 + 4 + 200 * (32 + 4 + 2048));
}
