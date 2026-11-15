//! Byte-compatibility with cryptsetup: `veritysetup format --no-superblock`
//! over the same data and salt must produce the same root hash and the same
//! hash tree, both standalone and in place behind our footer.
//!
//! Needs `veritysetup` (in the dev shell); skipped with a note otherwise.

use std::path::Path;
use std::process::Command;

use ed25519_dalek::SigningKey;
use pinna_image::{keys, verity};

fn have_veritysetup() -> bool {
    let ok = Command::new("veritysetup")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("SKIPPED: veritysetup not found (run inside `nix develop`)");
    }
    ok
}

fn veritysetup(args: &[&str]) -> String {
    let out = Command::new("veritysetup").args(args).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "veritysetup {args:?} failed: {text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    text
}

fn root_hash(output: &str) -> String {
    output
        .lines()
        .find_map(|l| l.strip_prefix("Root hash:"))
        .expect("no root hash in veritysetup output")
        .trim()
        .to_owned()
}

/// Data with variety: pseudo-random blocks, repeated blocks and zero blocks.
fn data(blocks: usize) -> Vec<u8> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut d = vec![0u8; blocks * 4096];
    for (i, chunk) in d.chunks_mut(4096).enumerate() {
        match i % 7 {
            3 => {}
            5 => chunk.fill(0xa5),
            _ => {
                for b in chunk.iter_mut() {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    *b = x as u8;
                }
            }
        }
    }
    d
}

fn format_args(salt: &str) -> Vec<&str> {
    vec![
        "format",
        "--no-superblock",
        "--format=1",
        "--hash=sha256",
        "--data-block-size=4096",
        "--hash-block-size=4096",
        "--salt",
        salt,
    ]
}

#[test]
fn hash_tree_matches_veritysetup() {
    if !have_veritysetup() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let salt = [0x5au8; 32];
    let salt_hex = keys::hex(&salt);
    // 1 block (no tree), 2 levels, exact level boundary, 3 levels
    for blocks in [1usize, 2, 128, 129, 300, 16385] {
        let d = data(blocks);
        let (dp, hp) = (dir.path().join("data"), dir.path().join("hash"));
        std::fs::write(&dp, &d).unwrap();
        let _ = std::fs::remove_file(&hp);
        let mut args = format_args(&salt_hex);
        args.extend([dp.to_str().unwrap(), hp.to_str().unwrap()]);
        let theirs = root_hash(&veritysetup(&args));
        let tree = pinna_image::hash_tree(&d[..], d.len() as u64, &salt).unwrap();
        assert_eq!(keys::hex(&tree.root), theirs, "root hash, {blocks} blocks");
        let their_tree = std::fs::read(&hp).unwrap();
        assert_eq!(
            their_tree.len() as u64,
            verity::tree_size(blocks as u64),
            "tree size, {blocks} blocks"
        );
        if blocks > 1 {
            assert!(their_tree == tree.bytes, "hash tree bytes, {blocks} blocks");
        }
    }
}

#[test]
fn sealed_image_matches_veritysetup_in_place() {
    if !have_veritysetup() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // A fake erofs: superblock + data.
    let blocks = 600u32;
    let mut img = data(blocks as usize);
    img[..2048].fill(0);
    img[1024..1028].copy_from_slice(&0xE0F5_E1E2u32.to_le_bytes());
    img[1024 + 12] = 12;
    img[1024 + 36..1024 + 40].copy_from_slice(&blocks.to_le_bytes());
    let salt = [0x11u8; 32];
    let key = SigningKey::from_bytes(&[8; 32]);
    let mut sealed = Vec::new();
    let f = pinna_image::seal(&img[..], "test-1", salt, &key, &mut sealed).unwrap();

    // veritysetup rewrites the tree region of a copy, in place behind the footer.
    let p = dir.path().join("slot.img");
    std::fs::write(&p, &sealed).unwrap();
    let hash_offset = f.hash_offset.to_string();
    let data_blocks = f.data_blocks().to_string();
    let salt_hex = keys::hex(&salt);
    let mut args = format_args(&salt_hex);
    let ps = p.to_str().unwrap();
    args.extend([
        "--hash-offset",
        &hash_offset,
        "--data-blocks",
        &data_blocks,
        ps,
        ps,
    ]);
    let theirs = root_hash(&veritysetup(&args));
    assert_eq!(theirs, keys::hex(&f.root_hash));
    assert!(
        std::fs::read(&p).unwrap() == sealed,
        "veritysetup's in-place tree differs from the sealed image"
    );
    // and veritysetup accepts the image as we sealed it
    std::fs::write(&p, &sealed).unwrap();
    veritysetup(&[
        "verify",
        "--no-superblock",
        "--format=1",
        "--hash=sha256",
        "--data-block-size=4096",
        "--hash-block-size=4096",
        "--salt",
        &salt_hex,
        "--hash-offset",
        &hash_offset,
        "--data-blocks",
        &data_blocks,
        ps,
        ps,
        &theirs,
    ]);
    assert!(Path::new(&p).exists());
}
