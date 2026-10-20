//! Records the encoded size of version 2 binary messages against a JSON-with-base64
//! candidate for the same fixture. Run with `--nocapture`.

mod support;

use javelin::{FileEntry, Manifest, RepoPath};
use javelin_protocol::{Kind, Message, codec::encode};
use support::{chunks, repo};

fn base64_len(n: usize) -> usize {
    n.div_ceil(3) * 4
}

/// {"v":2,"t":"objects","objects":[{"kind":..,"id":..,"data":..}]}
fn json_objects(objects: &[(&str, javelin::ObjectId, usize)]) -> usize {
    let items: Vec<String> = objects
        .iter()
        .map(|(kind, id, len)| {
            format!(
                r#"{{"kind":"{kind}","id":"{id}","data":"{}"}}"#,
                "A".repeat(base64_len(*len))
            )
        })
        .collect();
    format!(r#"{{"v":2,"t":"objects","objects":[{}]}}"#, items.join(",")).len()
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
        blobs.push((Kind::Blob, id, bytes));
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
    let root = source.put_snapshot(&manifest).unwrap();
    let trees: Vec<_> = chunks(&source, root)
        .into_iter()
        .map(|(id, bytes)| (Kind::Tree, id, bytes))
        .collect();
    let raw: usize = blobs.iter().map(|(_, _, b)| b.len()).sum();
    let binary_blobs = encode(&Message::Objects(blobs.clone())).unwrap().len();
    let binary_trees = encode(&Message::Objects(trees.clone())).unwrap().len();
    let json = |items: &[(Kind, javelin::ObjectId, Vec<u8>)], kind| {
        json_objects(
            &items
                .iter()
                .map(|(_, id, b)| (kind, *id, b.len()))
                .collect::<Vec<_>>(),
        )
    };
    let (json_b, json_t) = (json(&blobs, "blob"), json(&trees, "tree"));
    println!(
        "fixture: 200 blobs x 2048 bytes = {raw} raw bytes, 200 entries in {} chunks",
        trees.len()
    );
    println!("blobs: binary {binary_blobs} json {json_b}");
    println!(
        "trees: binary {binary_trees} json {json_t} canonical manifest {}",
        manifest.canonical_bytes().len()
    );
    assert!(binary_blobs < json_b && binary_trees < json_t);
    assert!(binary_trees < manifest.canonical_bytes().len());
    assert_eq!(binary_blobs, 1 + 4 + 200 * (1 + 32 + 4 + 2048));
}
