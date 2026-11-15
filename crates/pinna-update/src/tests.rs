use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use ed25519_dalek::SigningKey;
use pinna_bootctl::slot::{SlotInfo, SlotState};

use super::*;

const PART: usize = 2 << 20;

struct Env {
    dir: tempfile::TempDir,
    opts: Options,
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[42; 32])
}

fn sealed(version: &str, blocks: u32, key: &SigningKey) -> Vec<u8> {
    let mut img = vec![0u8; blocks as usize * 4096];
    img[1024..1028].copy_from_slice(&0xE0F5_E1E2u32.to_le_bytes());
    img[1024 + 12] = 12;
    img[1024 + 36..1024 + 40].copy_from_slice(&blocks.to_le_bytes());
    for (i, b) in img.iter_mut().enumerate().skip(4096) {
        *b = (i % 253) as u8;
    }
    let mut out = Vec::new();
    pinna_image::seal(&img[..], version, [7; 32], key, &mut out).unwrap();
    out
}

fn env(booted: &str, state: Option<SlotState>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let p = |n: &str| dir.path().join(n);
    std::fs::create_dir_all(p("parts")).unwrap();
    std::fs::create_dir_all(p("run")).unwrap();
    std::fs::write(p("parts/system_a"), vec![0xaa; PART]).unwrap();
    std::fs::write(p("parts/system_b"), vec![0xbb; PART]).unwrap();
    std::fs::write(p("misc"), vec![0u8; 0x80000]).unwrap();
    std::fs::write(p("run/boot-slot"), format!("{booted}\n")).unwrap();
    std::fs::write(p("run/boot-version"), "old\n").unwrap();
    if let Some(s) = state {
        slot::write_state(&p("misc"), &s).unwrap();
    }
    let opts = Options {
        tries: 3,
        misc: p("misc"),
        partdir: p("parts"),
        run_dir: p("run"),
        key: key().verifying_key(),
    };
    Env { dir, opts }
}

impl Env {
    fn path(&self, n: &str) -> PathBuf {
        self.dir.path().join(n)
    }

    /// Publish an image + manifest under `rel/`; returns the manifest path.
    fn publish(&self, image: &[u8], version: &str, sha: Option<&str>) -> PathBuf {
        let rel = self.path("rel");
        std::fs::create_dir_all(&rel).unwrap();
        std::fs::write(rel.join("img.bin"), image).unwrap();
        let sha = sha.map_or_else(|| keys::hex(&Sha256::digest(image)), str::to_owned);
        let m = format!(
            r#"{{"version":"{version}","root":{{"url":"img.bin","size":{},"sha256":"{sha}"}}}}"#,
            image.len()
        );
        std::fs::write(rel.join("manifest.json"), m).unwrap();
        rel.join("manifest.json")
    }

    fn state(&self) -> Option<SlotState> {
        slot::read_state(&self.opts.misc).unwrap()
    }

    fn part(&self, n: &str) -> Vec<u8> {
        std::fs::read(self.path("parts").join(n)).unwrap()
    }
}

fn st(active: Slot, a: (u8, bool), b: (u8, bool)) -> SlotState {
    SlotState {
        active,
        slots: [
            SlotInfo {
                tries_left: a.0,
                successful: a.1,
            },
            SlotInfo {
                tries_left: b.0,
                successful: b.1,
            },
        ],
    }
}

fn quiet() -> impl FnMut(&str) {
    |_| {}
}

#[test]
fn installs_into_the_other_slot() {
    let e = env("a", None);
    let img = sealed("2.0", 100, &key());
    let m = e.publish(&img, "2.0", None);
    let got = install(&e.opts, m.to_str().unwrap(), &mut quiet()).unwrap();
    assert_eq!((got.slot, got.version.as_str()), (Slot::B, "2.0"));
    let b = e.part("system_b");
    assert_eq!(&b[..img.len()], &img[..]);
    assert!(e.part("system_a").iter().all(|&x| x == 0xaa));
    assert_eq!(e.state(), Some(st(Slot::B, (0, true), (3, false))));
}

#[test]
fn file_url_and_slot_b_to_a() {
    let mut e = env("b", Some(st(Slot::B, (0, true), (0, true))));
    e.opts.tries = 1;
    let img = sealed("3.0", 64, &key());
    let m = e.publish(&img, "3.0", None);
    let url = Url::from_file_path(&m).unwrap();
    install(&e.opts, url.as_str(), &mut quiet()).unwrap();
    assert_eq!(&e.part("system_a")[..img.len()], &img[..]);
    assert_eq!(e.state(), Some(st(Slot::A, (1, false), (0, true))));
}

#[test]
fn refuses_during_a_trial_boot() {
    let e = env("b", Some(st(Slot::B, (0, true), (2, false))));
    let m = e.publish(&sealed("2.0", 64, &key()), "2.0", None);
    let err = install(&e.opts, m.to_str().unwrap(), &mut quiet()).unwrap_err();
    assert!(format!("{err:#}").contains("not marked good"), "{err:#}");
    assert!(e.part("system_a").iter().all(|&x| x == 0xaa));
    assert_eq!(e.state(), Some(st(Slot::B, (0, true), (2, false))));
}

#[test]
fn refuses_without_pinna_init() {
    let e = env("a", None);
    std::fs::remove_file(e.path("run/boot-slot")).unwrap();
    let m = e.publish(&sealed("2.0", 64, &key()), "2.0", None);
    assert!(install(&e.opts, m.to_str().unwrap(), &mut quiet()).is_err());
}

#[test]
fn bad_signature_leaves_the_target_unbootable() {
    let e = env("a", Some(st(Slot::A, (0, true), (0, true))));
    let img = sealed("2.0", 64, &SigningKey::from_bytes(&[1; 32]));
    let m = e.publish(&img, "2.0", None);
    let err = install(&e.opts, m.to_str().unwrap(), &mut quiet()).unwrap_err();
    assert!(format!("{err:#}").contains("signature"), "{err:#}");
    assert_eq!(e.state(), Some(st(Slot::A, (0, true), (0, false))));
}

#[test]
fn checksum_size_version_and_fit() {
    let e = env("a", None);
    let img = sealed("2.0", 64, &key());
    let m = e.publish(&img, "2.0", Some(&"0".repeat(64)));
    let err = install(&e.opts, m.to_str().unwrap(), &mut quiet()).unwrap_err();
    assert!(format!("{err:#}").contains("sha256 mismatch"), "{err:#}");

    let m = e.publish(&img, "2.1", None);
    let err = install(&e.opts, m.to_str().unwrap(), &mut quiet()).unwrap_err();
    assert!(
        format!("{err:#}").contains("does not match the manifest"),
        "{err:#}"
    );

    let big = sealed("2.0", (PART / 4096) as u32, &key());
    let m = e.publish(&big, "2.0", None);
    let err = install(&e.opts, m.to_str().unwrap(), &mut quiet()).unwrap_err();
    assert!(format!("{err:#}").contains("does not fit"), "{err:#}");
    // nothing became active
    assert_eq!(e.state().map(|s| s.active), Some(Slot::A));
}

/// A one-connection-per-request HTTP/1.0 server for `n` requests.
fn serve(root: PathBuf, n: usize) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}/", l.local_addr().unwrap());
    let h = std::thread::spawn(move || {
        let mut paths = Vec::new();
        for s in l.incoming().take(n) {
            let mut s = s.unwrap();
            let mut line = String::new();
            let mut r = BufReader::new(s.try_clone().unwrap());
            r.read_line(&mut line).unwrap();
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
            }
            let path = line.split_whitespace().nth(1).unwrap().to_owned();
            let body = std::fs::read(root.join(path.trim_start_matches('/'))).unwrap();
            write!(
                s,
                "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .unwrap();
            s.write_all(&body).unwrap();
            paths.push(path);
        }
        paths
    });
    (addr, h)
}

#[test]
fn over_http_with_a_relative_image_url() {
    let e = env("a", None);
    let img = sealed("2.0", 64, &key());
    e.publish(&img, "2.0", None);
    let (base, server) = serve(e.path(""), 2);
    install(&e.opts, &format!("{base}rel/manifest.json"), &mut quiet()).unwrap();
    assert_eq!(
        server.join().unwrap(),
        ["/rel/manifest.json", "/rel/img.bin"]
    );
    assert_eq!(&e.part("system_b")[..img.len()], &img[..]);
}
