use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use ed25519_dalek::SigningKey;
use pinna_bootctl::bcb::Record;

use super::*;

const PART: usize = 2 << 20;
const BOOT_PART: usize = 1 << 20;

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

/// A boot image as LK sees it: the header's magic and command line, then
/// `fill`.
fn bootimg(fill: u8, len: usize) -> Vec<u8> {
    let mut v = vec![fill; len];
    v[..2048].fill(0);
    v[..8].copy_from_slice(b"ANDROID!");
    let line = b"bootopt=64S3,32N2,32N2";
    v[64..64 + line.len()].copy_from_slice(line);
    v
}

fn tagged(mut img: Vec<u8>, s: char) -> Vec<u8> {
    pinna_bootimg::tag_slot(&mut img, s).unwrap();
    img
}

fn rec(a: u8, b: u8) -> Record {
    Record::decode(&[0, b'A', b'B', b'B', 1, a, b]).unwrap()
}

fn env(booted: &str, record: Option<Record>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let p = |n: &str| dir.path().join(n);
    std::fs::create_dir_all(p("parts")).unwrap();
    std::fs::create_dir_all(p("run")).unwrap();
    std::fs::write(p("parts/system_a"), vec![0xaa; PART]).unwrap();
    std::fs::write(p("parts/system_b"), vec![0xbb; PART]).unwrap();
    std::fs::write(p("misc"), vec![0u8; 0x80000]).unwrap();
    std::fs::write(p("run/boot-slot"), format!("{booted}\n")).unwrap();
    std::fs::write(p("run/boot-version"), "old\n").unwrap();
    if let Some(r) = record {
        bcb::write(&p("misc"), &r).unwrap();
    }
    let opts = Options {
        attempts: 3,
        misc: p("misc"),
        partdir: p("parts"),
        run_dir: p("run"),
        kernel_slots: p("kernel-slots"),
        key: key().verifying_key(),
    };
    Env { dir, opts }
}

impl Env {
    fn path(&self, n: &str) -> PathBuf {
        self.dir.path().join(n)
    }

    /// Boot partitions, as on a device: boot_a holds the running kernel.
    fn with_boot_parts(&self) {
        let mut a = tagged(bootimg(0xa0, 8192), 'a');
        a.resize(BOOT_PART, 0);
        std::fs::write(self.path("parts/boot_a"), a).unwrap();
        std::fs::write(self.path("parts/boot_b"), vec![0xbb; BOOT_PART]).unwrap();
    }

    /// Publish an image + manifest under `rel/`; returns the manifest path.
    fn publish(&self, image: &[u8], version: &str, sha: Option<&str>) -> PathBuf {
        self.publish_with(image, version, sha, None)
    }

    /// The same, with a boot image and the sha256 its entry claims.
    fn publish_with(
        &self,
        image: &[u8],
        version: &str,
        sha: Option<&str>,
        boot: Option<(&[u8], Option<&str>)>,
    ) -> PathBuf {
        let rel = self.path("rel");
        std::fs::create_dir_all(&rel).unwrap();
        std::fs::write(rel.join("img.bin"), image).unwrap();
        let hex = |d: &[u8]| keys::hex(&Sha256::digest(d));
        let sha = sha.map_or_else(|| hex(image), str::to_owned);
        let boot = boot.map_or_else(String::new, |(b, bsha)| {
            std::fs::write(rel.join("boot.img"), b).unwrap();
            let bsha = bsha.map_or_else(|| hex(b), str::to_owned);
            format!(
                r#","boot":{{"url":"boot.img","size":{},"sha256":"{bsha}"}}"#,
                b.len()
            )
        });
        let m = format!(
            r#"{{"version":"{version}","root":{{"url":"img.bin","size":{},"sha256":"{sha}"}}{boot}}}"#,
            image.len()
        );
        std::fs::write(rel.join("manifest.json"), m).unwrap();
        rel.join("manifest.json")
    }

    fn record(&self) -> Option<Record> {
        bcb::read(&self.opts.misc).unwrap()
    }

    fn part(&self, n: &str) -> Vec<u8> {
        std::fs::read(self.path("parts").join(n)).unwrap()
    }

    fn install(&self, manifest: &Path) -> Result<Vec<String>> {
        let mut lines = Vec::new();
        install(&self.opts, manifest.to_str().unwrap(), &mut |l| {
            lines.push(l.to_owned())
        })?;
        Ok(lines)
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
    let lines = e.install(&m).unwrap();
    let b = e.part("system_b");
    assert_eq!(&b[..img.len()], &img[..]);
    assert!(e.part("system_a").iter().all(|&x| x == 0xaa));
    // A good at 14, B on trial at 15 with 3 attempts (4 tries).
    assert_eq!(e.record(), Some(rec(0x8e, 0x4f)));
    assert!(
        lines.iter().any(|l| l.contains("the kernel stays")),
        "{lines:?}"
    );
}

#[test]
fn file_url_and_slot_b_to_a() {
    let mut e = env("b", Some(rec(0x8e, 0x8f)));
    e.opts.attempts = 1;
    let img = sealed("3.0", 64, &key());
    let m = e.publish(&img, "3.0", None);
    let url = Url::from_file_path(&m).unwrap();
    install(&e.opts, url.as_str(), &mut quiet()).unwrap();
    assert_eq!(&e.part("system_a")[..img.len()], &img[..]);
    // A on trial at 15 with 1 attempt (2 tries), B good at 14.
    assert_eq!(e.record(), Some(rec(0x2f, 0x8e)));
}

#[test]
fn refuses_during_a_trial_boot() {
    let e = env("b", Some(rec(0x8e, 0x3f)));
    let m = e.publish(&sealed("2.0", 64, &key()), "2.0", None);
    let err = install(&e.opts, m.to_str().unwrap(), &mut quiet()).unwrap_err();
    assert!(format!("{err:#}").contains("not marked good"), "{err:#}");
    assert!(e.part("system_a").iter().all(|&x| x == 0xaa));
    assert_eq!(e.record(), Some(rec(0x8e, 0x3f)));
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
    let e = env("a", Some(rec(0x8f, 0x8e)));
    let img = sealed("2.0", 64, &SigningKey::from_bytes(&[1; 32]));
    let m = e.publish(&img, "2.0", None);
    let err = install(&e.opts, m.to_str().unwrap(), &mut quiet()).unwrap_err();
    assert!(format!("{err:#}").contains("signature"), "{err:#}");
    assert_eq!(e.record(), Some(rec(0x8f, 0x00)));
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
    assert_eq!(e.record().and_then(|r| r.next_boot()), Some(Slot::A));
}

#[test]
fn the_manifests_kernel_goes_into_boot_b() {
    let e = env("a", None);
    e.with_boot_parts();
    let boot_a = e.part("boot_a");
    let kernel = bootimg(0xcc, 8192);
    let img = sealed("2.0", 64, &key());
    let m = e.publish_with(&img, "2.0", None, Some((&kernel, None)));
    let lines = e.install(&m).unwrap();
    let b = e.part("boot_b");
    assert_eq!(&b[..kernel.len()], &tagged(kernel.clone(), 'b')[..]);
    assert_eq!(pinna_bootimg::slot_tag(&b), Some('b'));
    assert_eq!(e.part("boot_a"), boot_a, "the running kernel stays");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("kernel from") && l.contains("boot_b")),
        "{lines:?}"
    );
    assert_eq!(e.record(), Some(rec(0x8e, 0x4f)));
}

#[test]
fn without_a_boot_image_the_running_kernel_goes_along() {
    let e = env("a", None);
    e.with_boot_parts();
    let img = sealed("2.0", 64, &key());
    e.install(&e.publish(&img, "2.0", None)).unwrap();
    assert_eq!(e.part("boot_b"), tagged(e.part("boot_a"), 'b'));
}

#[test]
fn boot_a_only_after_a_boot_from_b_showed_the_bootloader_loads_it() {
    let e = env("b", Some(rec(0x8e, 0x8f)));
    e.with_boot_parts();
    let boot_a = e.part("boot_a");
    let kernel = bootimg(0xdd, 8192);
    let img = sealed("2.0", 64, &key());
    let m = e.publish_with(&img, "2.0", None, Some((&kernel, None)));
    let lines = e.install(&m).unwrap();
    assert_eq!(e.part("boot_a"), boot_a, "no proof yet: boot_a stays");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("the kernel in boot_a stays")),
        "{lines:?}"
    );
    assert_eq!(&e.part("system_a")[..img.len()], &img[..]);

    // mark-good saw a boot of b load boot_b.
    slot::KernelSlots::Honoured
        .write(&e.opts.kernel_slots)
        .unwrap();
    e.install(&m).unwrap();
    assert_eq!(&e.part("boot_a")[..kernel.len()], &tagged(kernel, 'a')[..]);
}

#[test]
fn a_bad_kernel_changes_nothing() {
    let e = env("a", Some(rec(0x8f, 0x8e)));
    e.with_boot_parts();
    let img = sealed("2.0", 64, &key());
    let kernel = bootimg(0xcc, 8192);
    let m = e.publish_with(&img, "2.0", None, Some((&kernel, Some(&"0".repeat(64)))));
    let err = install(&e.opts, m.to_str().unwrap(), &mut quiet()).unwrap_err();
    assert!(format!("{err:#}").contains("sha256 mismatch"), "{err:#}");
    assert_eq!(e.record(), Some(rec(0x8f, 0x8e)), "slot b still counts");
    assert!(e.part("system_b").iter().all(|&x| x == 0xbb));
    assert!(e.part("boot_b").iter().all(|&x| x == 0xbb));
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
