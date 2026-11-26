use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use ed25519_dalek::SigningKey;
use pinna_bootctl::bcb::Record;

use pinna_bootctl::storage::Parts;
use pinna_image::version::Version;

use super::*;
use pinna_board::BootFormat;

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
        parts: Parts::in_dir(p("parts")),
        run_dir: p("run"),
        kernel_slots: p("kernel-slots"),
        key: key().verifying_key(),
        model: Some("echo-dot-2".into()),
        board: None,
        dev_overlay: p("pinna-dev"),
        volume_update: Arc::new(UbiIoctl),
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

    /// Publish an image + signed manifest under `rel/`; returns the manifest path.
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
        let m = self.manifest(image, version, sha, boot);
        self.publish_json(image, &m, Some(&key()))
    }

    /// A manifest as `pinna-image manifest` writes it.
    fn manifest(
        &self,
        image: &[u8],
        version: &str,
        sha: Option<&str>,
        boot: Option<(&[u8], Option<&str>)>,
    ) -> serde_json::Value {
        let rel = self.path("rel");
        std::fs::create_dir_all(&rel).unwrap();
        let hex = |d: &[u8]| keys::hex(&Sha256::digest(d));
        let sha = sha.map_or_else(|| hex(image), str::to_owned);
        let channel = Version::parse(version).map_or("stable", |v| v.channel());
        let mut m = serde_json::json!({
            "version": version,
            "channel": channel,
            "summary": format!("Pinna {version}."),
            "requires": { "boot_level": 1 },
            "root": { "url": "img.bin", "size": image.len(), "sha256": sha },
        });
        if let Some((b, bsha)) = boot {
            std::fs::write(rel.join("boot.img"), b).unwrap();
            let bsha = bsha.map_or_else(|| hex(b), str::to_owned);
            m["boot"] = serde_json::json!({ "url": "boot.img", "size": b.len(), "sha256": bsha });
        }
        m
    }

    /// Write `image`, the manifest and, signed with `signer`, its signature.
    fn publish_json(
        &self,
        image: &[u8],
        m: &serde_json::Value,
        signer: Option<&SigningKey>,
    ) -> PathBuf {
        let rel = self.path("rel");
        std::fs::create_dir_all(&rel).unwrap();
        std::fs::write(rel.join("img.bin"), image).unwrap();
        let bytes = serde_json::to_vec_pretty(m).unwrap();
        std::fs::write(rel.join("manifest.json"), &bytes).unwrap();
        let sig = rel.join("manifest.json.sig");
        match signer {
            Some(k) => std::fs::write(&sig, pinna_image::manifest::sign(&bytes, k)).unwrap(),
            None => {
                let _ = std::fs::remove_file(&sig);
            }
        }
        rel.join("manifest.json")
    }

    fn record(&self) -> Option<Record> {
        bcb::read(&self.opts.misc).unwrap()
    }

    fn part(&self, n: &str) -> Vec<u8> {
        std::fs::read(self.path("parts").join(n)).unwrap()
    }

    fn install(&self, manifest: &Path) -> Result<Vec<String>> {
        self.install_with(manifest, &Rules::default())
    }

    fn install_with(&self, manifest: &Path, rules: &Rules) -> Result<Vec<String>> {
        let mut lines = Vec::new();
        install(&self.opts, manifest.to_str().unwrap(), rules, &mut |l| {
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
    let img = sealed("2.0.0", 100, &key());
    let m = e.publish(&img, "2.0.0", None);
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
    let img = sealed("3.0.0", 64, &key());
    let m = e.publish(&img, "3.0.0", None);
    let url = Url::from_file_path(&m).unwrap();
    install(&e.opts, url.as_str(), &Rules::default(), &mut quiet()).unwrap();
    assert_eq!(&e.part("system_a")[..img.len()], &img[..]);
    // A on trial at 15 with 1 attempt (2 tries), B good at 14.
    assert_eq!(e.record(), Some(rec(0x2f, 0x8e)));
}

#[test]
fn refuses_during_a_trial_boot() {
    let e = env("b", Some(rec(0x8e, 0x3f)));
    let m = e.publish(&sealed("2.0.0", 64, &key()), "2.0.0", None);
    let err = e.install_with(&m, &Rules::default()).unwrap_err();
    assert!(format!("{err:#}").contains("not marked good"), "{err:#}");
    assert!(e.part("system_a").iter().all(|&x| x == 0xaa));
    assert_eq!(e.record(), Some(rec(0x8e, 0x3f)));
}

#[test]
fn refuses_without_pinna_init() {
    let e = env("a", None);
    std::fs::remove_file(e.path("run/boot-slot")).unwrap();
    let m = e.publish(&sealed("2.0.0", 64, &key()), "2.0.0", None);
    assert!(e.install_with(&m, &Rules::default()).is_err());
}

#[test]
fn bad_signature_leaves_the_target_unbootable() {
    let e = env("a", Some(rec(0x8f, 0x8e)));
    let img = sealed("2.0.0", 64, &SigningKey::from_bytes(&[1; 32]));
    let m = e.publish(&img, "2.0.0", None);
    let err = e.install_with(&m, &Rules::default()).unwrap_err();
    assert!(format!("{err:#}").contains("signature"), "{err:#}");
    assert_eq!(e.record(), Some(rec(0x8f, 0x00)));
}

#[test]
fn checksum_size_version_and_fit() {
    let e = env("a", None);
    let img = sealed("2.0.0", 64, &key());
    let m = e.publish(&img, "2.0.0", Some(&"0".repeat(64)));
    let err = e.install_with(&m, &Rules::default()).unwrap_err();
    assert!(format!("{err:#}").contains("sha256 mismatch"), "{err:#}");

    let m = e.publish(&img, "2.1.0", None);
    let err = e.install_with(&m, &Rules::default()).unwrap_err();
    assert!(
        format!("{err:#}").contains("does not match the manifest"),
        "{err:#}"
    );

    let big = sealed("2.0.0", (PART / 4096) as u32, &key());
    let m = e.publish(&big, "2.0.0", None);
    let err = e.install_with(&m, &Rules::default()).unwrap_err();
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
    let img = sealed("2.0.0", 64, &key());
    let m = e.publish_with(&img, "2.0.0", None, Some((&kernel, None)));
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
    let img = sealed("2.0.0", 64, &key());
    e.install(&e.publish(&img, "2.0.0", None)).unwrap();
    assert_eq!(e.part("boot_b"), tagged(e.part("boot_a"), 'b'));
}

#[test]
fn boot_a_only_after_a_boot_from_b_showed_the_bootloader_loads_it() {
    let e = env("b", Some(rec(0x8e, 0x8f)));
    e.with_boot_parts();
    let boot_a = e.part("boot_a");
    let kernel = bootimg(0xdd, 8192);
    let img = sealed("2.0.0", 64, &key());
    let m = e.publish_with(&img, "2.0.0", None, Some((&kernel, None)));
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
fn a_bootloader_that_ignores_the_slot_gets_no_kernel() {
    let e = env("a", None);
    e.with_boot_parts();
    slot::KernelSlots::Ignored
        .write(&e.opts.kernel_slots)
        .unwrap();
    let img = sealed("2.0.0", 64, &key());
    let m = e.publish_with(&img, "2.0.0", None, Some((&bootimg(0xcc, 8192), None)));
    let lines = e.install(&m).unwrap();
    assert!(
        e.part("boot_b").iter().all(|&x| x == 0xbb),
        "boot_b untouched"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("loads boot_a whichever slot")),
        "{lines:?}"
    );
    assert_eq!(&e.part("system_b")[..img.len()], &img[..]);
}

#[test]
fn a_bad_kernel_changes_nothing() {
    let e = env("a", Some(rec(0x8f, 0x8e)));
    e.with_boot_parts();
    let img = sealed("2.0.0", 64, &key());
    let kernel = bootimg(0xcc, 8192);
    let m = e.publish_with(&img, "2.0.0", None, Some((&kernel, Some(&"0".repeat(64)))));
    let err = e.install_with(&m, &Rules::default()).unwrap_err();
    assert!(format!("{err:#}").contains("sha256 mismatch"), "{err:#}");
    assert_eq!(e.record(), Some(rec(0x8f, 0x8e)), "slot b still counts");
    assert!(e.part("system_b").iter().all(|&x| x == 0xbb));
    assert!(e.part("boot_b").iter().all(|&x| x == 0xbb));
}

/// A one-connection-per-request HTTP/1.0 server for `n` requests: files
/// under `root`, 404 for missing ones, and 302 for the paths in `redirects`.
fn serve(
    root: PathBuf,
    n: usize,
    redirects: &[(&str, &str)],
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}/", l.local_addr().unwrap());
    let redirects: Vec<(String, String)> = redirects
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect();
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
            if let Some((_, to)) = redirects.iter().find(|(from, _)| *from == path) {
                write!(
                    s,
                    "HTTP/1.0 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\n\r\n"
                )
                .unwrap();
            } else if let Ok(body) = std::fs::read(root.join(path.trim_start_matches('/'))) {
                write!(
                    s,
                    "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .unwrap();
                s.write_all(&body).unwrap();
            } else {
                let body = "No release on stable yet.";
                write!(
                    s,
                    "HTTP/1.0 404 Not Found\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            paths.push(path);
        }
        paths
    });
    (addr, h)
}

#[test]
fn over_http_with_a_relative_image_url() {
    let e = env("a", None);
    let img = sealed("2.0.0", 64, &key());
    e.publish(&img, "2.0.0", None);
    let (base, server) = serve(e.path(""), 3, &[]);
    install(
        &e.opts,
        &format!("{base}rel/manifest.json"),
        &Rules::default(),
        &mut quiet(),
    )
    .unwrap();
    assert_eq!(
        server.join().unwrap(),
        [
            "/rel/manifest.json",
            "/rel/manifest.json.sig",
            "/rel/img.bin"
        ]
    );
    assert_eq!(&e.part("system_b")[..img.len()], &img[..]);
}

#[test]
fn a_channel_redirect_resolves_files_next_to_the_manifest() {
    let e = env("a", None);
    let img = sealed("2.0.0", 64, &key());
    e.publish(&img, "2.0.0", None);
    // As the release service: /v1/channels/stable → the release's manifest.
    let (base, server) = serve(
        e.path(""),
        6,
        &[("/v1/channels/stable", "/rel/manifest.json")],
    );
    let checked = check(&e.opts, &format!("{base}v1/channels/stable")).unwrap();
    assert_eq!(checked.fetched.url.path(), "/rel/manifest.json");
    assert_eq!(checked.fetched.manifest.version, "2.0.0");
    assert_eq!(checked.fetched.manifest.summary, "Pinna 2.0.0.");
    assert_eq!(checked.blocked, None);
    let rules = Rules {
        channel: Some(Channel::Stable),
        expect_version: Some("2.0.0".into()),
    };
    install(&e.opts, checked.fetched.url.as_str(), &rules, &mut quiet()).unwrap();
    assert_eq!(
        server.join().unwrap(),
        [
            "/v1/channels/stable",
            "/rel/manifest.json",
            "/rel/manifest.json.sig",
            "/rel/manifest.json",
            "/rel/manifest.json.sig",
            "/rel/img.bin"
        ]
    );
    assert_eq!(&e.part("system_b")[..img.len()], &img[..]);
}

#[test]
fn a_failed_check_says_why() {
    let e = env("a", None);
    let (base, server) = serve(e.path(""), 1, &[]);
    let err = check(&e.opts, &format!("{base}v1/channels/stable")).unwrap_err();
    assert!(
        matches!(&err, FetchError::Status { status: 404, body, .. } if body == "No release on stable yet."),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "127.0.0.1 answered 404: No release on stable yet."
    );
    server.join().unwrap();

    // Nothing listens there.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let err = check(&e.opts, &format!("http://{addr}/manifest.json")).unwrap_err();
    assert_eq!(err.kind(), "no_answer", "{err:?}");
}

#[test]
fn refuses_a_manifest_without_a_valid_signature() {
    let e = env("a", Some(rec(0x8f, 0x8e)));
    let img = sealed("2.0.0", 64, &key());
    let m = e.manifest(&img, "2.0.0", None, None);

    let path = e.publish_json(&img, &m, None);
    let err = check(&e.opts, path.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, FetchError::NoSignature), "{err:?}");
    assert!(e.install(&path).is_err());

    let path = e.publish_json(&img, &m, Some(&SigningKey::from_bytes(&[1; 32])));
    let err = check(&e.opts, path.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, FetchError::BadSignature), "{err:?}");

    // Signed, then changed.
    let path = e.publish_json(&img, &m, Some(&key()));
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("2.0.0", "2.0.1");
    std::fs::write(&path, text).unwrap();
    let err = e.install(&path).unwrap_err();
    assert!(format!("{err:#}").contains("signature"), "{err:#}");

    // Nothing was touched.
    assert_eq!(e.record(), Some(rec(0x8f, 0x8e)));
    assert!(e.part("system_b").iter().all(|&x| x == 0xbb));
}

#[test]
fn refuses_a_manifest_that_breaks_the_format() {
    let e = env("a", None);
    let img = sealed("2.0.0-beta.1", 64, &key());
    let mut m = e.manifest(&img, "2.0.0-beta.1", None, None);
    m["channel"] = "stable".into();
    let path = e.publish_json(&img, &m, Some(&key()));
    let err = check(&e.opts, path.to_str().unwrap()).unwrap_err();
    assert!(err.to_string().contains("belongs on beta"), "{err}");

    let img = sealed("2026.11.17.1405-abc1234", 64, &key());
    let m = e.manifest(&img, "2026.11.17.1405-abc1234", None, None);
    let path = e.publish_json(&img, &m, Some(&key()));
    let err = check(&e.opts, path.to_str().unwrap()).unwrap_err();
    assert!(err.to_string().contains("not a semantic version"), "{err}");
}

#[test]
fn a_channel_never_goes_back_and_stable_takes_no_beta() {
    let e = env("a", None);
    std::fs::write(e.path("run/boot-version"), "2.0.0\n").unwrap();
    let stable = Rules {
        channel: Some(Channel::Stable),
        expect_version: None,
    };
    let older = e.publish(&sealed("1.9.0", 64, &key()), "1.9.0", None);
    let err = e.install_with(&older, &stable).unwrap_err();
    assert!(format!("{err:#}").contains("never goes back"), "{err:#}");
    let same = e.publish(&sealed("2.0.0", 64, &key()), "2.0.0", None);
    assert!(e.install_with(&same, &stable).is_err());
    let beta = e.publish(&sealed("2.1.0-beta.1", 64, &key()), "2.1.0-beta.1", None);
    let err = e.install_with(&beta, &stable).unwrap_err();
    assert!(format!("{err:#}").contains("is a beta"), "{err:#}");
    assert_eq!(e.record(), None, "nothing written");

    // The beta channel takes it; given by hand, an older one installs too.
    let beta_rules = Rules {
        channel: Some(Channel::Beta),
        expect_version: None,
    };
    e.install_with(&beta, &beta_rules).unwrap();
    let older = e.publish(&sealed("1.9.0", 64, &key()), "1.9.0", None);
    let lines = e.install(&older).unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("older than the running 2.0.0")),
        "{lines:?}"
    );
}

#[test]
fn only_the_version_a_check_showed() {
    let e = env("a", None);
    let m = e.publish(&sealed("2.0.1", 64, &key()), "2.0.1", None);
    let rules = Rules {
        channel: None,
        expect_version: Some("2.0.0".into()),
    };
    let err = e.install_with(&m, &rules).unwrap_err();
    assert!(format!("{err:#}").contains("Check again"), "{err:#}");
    assert_eq!(e.record(), None);
}

#[test]
fn a_release_for_another_model_is_refused() {
    let mut e = env("a", None);
    let img = sealed("2.0.0", 64, &key());
    let mut m = e.manifest(&img, "2.0.0", None, None);
    m["requires"]["models"] = serde_json::json!(["echo-2"]);
    let path = e.publish_json(&img, &m, Some(&key()));
    let blocked = check(&e.opts, path.to_str().unwrap()).unwrap().blocked;
    assert_eq!(blocked.as_ref().map(Blocked::kind), Some("model"));
    assert_eq!(
        blocked.unwrap().to_string(),
        "This release is for the Echo (2nd gen), not for the Echo Dot (2nd gen)."
    );
    assert!(e.install(&path).is_err());
    e.opts.model = None;
    assert!(e.install(&path).is_err());
    e.opts.model = Some("echo-2".into());
    e.install(&path).unwrap();
}

#[test]
fn a_development_overlay_stops_the_install() {
    let e = env("a", None);
    std::fs::create_dir_all(e.path("pinna-dev")).unwrap();
    let m = e.publish(&sealed("2.0.0", 64, &key()), "2.0.0", None);
    let err = e.install(&m).unwrap_err();
    assert!(
        format!("{err:#}").contains("scripts/push-dev.sh --remove"),
        "{err:#}"
    );
    let blocked = check(&e.opts, m.to_str().unwrap()).unwrap().blocked;
    assert_eq!(blocked.as_ref().map(Blocked::kind), Some("overlay"));
    assert_eq!(e.record(), None);
}

/// A boot image of `level`, as `pinna-bootimg pack` writes it.
fn leveled(fill: u8, level: u32) -> Vec<u8> {
    let mut v = bootimg(fill, 8192);
    let line = format!("bootopt=64S3,32N2,32N2 pinna.level={level}");
    v[64..64 + line.len()].copy_from_slice(line.as_bytes());
    v
}

#[test]
fn the_boot_level_of_the_kernel_that_starts_the_slot() {
    let needs_two = |e: &Env, img: &[u8], boot: Option<&[u8]>| {
        let mut m = e.manifest(img, "2.0.0", None, boot.map(|b| (b, None)));
        m["requires"]["boot_level"] = 2.into();
        e.publish_json(img, &m, Some(&key()))
    };
    let img = sealed("2.0.0", 64, &key());

    // The kernel stays (this bootloader ignores the slot): boot_a's level counts.
    let e = env("a", Some(rec(0x8f, 0x8e)));
    e.with_boot_parts();
    slot::KernelSlots::Ignored
        .write(&e.opts.kernel_slots)
        .unwrap();
    let m = needs_two(&e, &img, Some(&leveled(0xcc, 2)));
    let blocked = check(&e.opts, m.to_str().unwrap()).unwrap().blocked;
    assert_eq!(blocked, Some(Blocked::BootLevel { needs: 2, has: 1 }));
    let err = e.install(&m).unwrap_err();
    assert!(
        format!("{err:#}").contains("install it over USB"),
        "{err:#}"
    );
    assert_eq!(e.record(), Some(rec(0x8f, 0x8e)), "slot b still counts");
    let mut a2 = tagged(leveled(0xa0, 2), 'a');
    a2.resize(BOOT_PART, 0);
    std::fs::write(e.path("parts/boot_a"), a2).unwrap();
    e.install(&m).unwrap();

    // The release brings a kernel of too low a level: refused after the
    // download, before anything is overwritten.
    let e = env("a", Some(rec(0x8f, 0x8e)));
    e.with_boot_parts();
    let m = needs_two(&e, &img, Some(&leveled(0xcc, 1)));
    assert_eq!(check(&e.opts, m.to_str().unwrap()).unwrap().blocked, None);
    let err = e.install(&m).unwrap_err();
    assert!(format!("{err:#}").contains("boot level 1"), "{err:#}");
    assert_eq!(e.record(), Some(rec(0x8f, 0x8e)));
    assert!(e.part("boot_b").iter().all(|&x| x == 0xbb));

    // No boot image in the release: the running kernel goes along, and its
    // level counts.
    let e = env("a", None);
    e.with_boot_parts();
    let m = needs_two(&e, &img, None);
    let blocked = check(&e.opts, m.to_str().unwrap()).unwrap().blocked;
    assert_eq!(blocked, Some(Blocked::BootLevel { needs: 2, has: 1 }));
}

/// A FIT as U-Boot reads it: the flattened device tree's magic, then `fill`.
fn fit(fill: u8, len: usize) -> Vec<u8> {
    let mut v = vec![fill; len];
    v[..4].copy_from_slice(&0xd00d_feedu32.to_be_bytes());
    v
}

/// The Echo Dot (5th gen): Amazon's U-Boot, FIT boot images.
fn env_fit(booted: &str) -> Env {
    let mut e = env(booted, None);
    e.opts.board = Some(pinna_board::Model::EchoDot5.board());
    e.opts.model = Some("echo-dot-5".into());
    std::fs::write(e.path("parts/boot_a"), vec![0xaa; BOOT_PART]).unwrap();
    std::fs::write(e.path("parts/boot_b"), vec![0xbb; BOOT_PART]).unwrap();
    e
}

/// A manifest whose boot images are `boots`: (models, image).
fn with_boots(
    e: &Env,
    img: &[u8],
    boot: Option<&[u8]>,
    boots: &[(&[&str], &[u8])],
) -> serde_json::Value {
    let mut m = e.manifest(img, "2.0.0", None, boot.map(|b| (b, None)));
    let rel = e.path("rel");
    let list: Vec<_> = boots
        .iter()
        .enumerate()
        .map(|(i, (models, b))| {
            std::fs::write(rel.join(format!("boot{i}.itb")), b).unwrap();
            serde_json::json!({
                "models": models,
                "url": format!("boot{i}.itb"),
                "size": b.len(),
                "sha256": keys::hex(&Sha256::digest(b)),
            })
        })
        .collect();
    if !list.is_empty() {
        m["boots"] = list.into();
    }
    m
}

#[test]
fn a_satellite_takes_the_boot_image_naming_its_model() {
    let e = env("a", None);
    let img = sealed("2.0.0", 64, &key());
    let (android, itb) = (bootimg(0xcc, 8192), fit(0xdd, 8192));
    let m = with_boots(&e, &img, Some(&android), &[(&["echo-dot-5"], &itb)]);
    let m: Manifest = serde_json::from_value(m).unwrap();
    let url = |a: Option<&Artifact>| a.map(|a| a.url.clone());
    use BootFormat::{Android, Fit};
    assert_eq!(
        url(m.boot_for(Some("echo-dot-5"), Fit)).as_deref(),
        Some("boot0.itb")
    );
    // Without an entry for its model, an Android bootloader falls back to
    // `boot`; a FIT one has nothing it could start.
    assert_eq!(
        url(m.boot_for(Some("echo-dot-2"), Android)).as_deref(),
        Some("boot.img")
    );
    assert_eq!(url(m.boot_for(Some("echo-dot-2"), Fit)), None);
    assert_eq!(url(m.boot_for(None, Fit)), None);
    // A manifest without `boots` still parses.
    let old: Manifest = serde_json::from_value(e.manifest(&img, "2.0.0", None, None)).unwrap();
    assert!(old.boots.is_empty() && old.boot_for(None, Android).is_none());
}

#[test]
fn on_a_fit_board_the_release_boot_image_goes_into_the_target_slots_volume() {
    for (booted, target) in [("a", "b"), ("b", "a")] {
        let mut e = env_fit(booted);
        if booted == "b" {
            bcb::write(&e.opts.misc, &rec(0x8e, 0x8f)).unwrap();
        }
        // None of the Android kernel rules applies, whatever they last saw.
        slot::KernelSlots::Ignored
            .write(&e.opts.kernel_slots)
            .unwrap();
        e.opts.attempts = 3;
        let img = sealed("2.0.0", 64, &key());
        let itb = fit(0xdd, 8192);
        let m = with_boots(&e, &img, None, &[(&["echo-dot-5"], &itb)]);
        let m = e.publish_json(&img, &m, Some(&key()));
        assert_eq!(check(&e.opts, m.to_str().unwrap()).unwrap().blocked, None);
        let lines = e.install(&m).unwrap();
        let boot = e.part(&format!("boot_{target}"));
        // Written as it is: no slot tag, and the other slot's volume stays.
        assert_eq!(&boot[..itb.len()], &itb[..], "{booted}");
        let other = e.part(&format!("boot_{booted}"));
        assert!(
            other
                .iter()
                .all(|&x| x == if booted == "a" { 0xaa } else { 0xbb })
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("kernel from") && l.contains(&format!("boot_{target}"))),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("the kernel stays")));
        assert_eq!(&e.part(&format!("system_{target}"))[..img.len()], &img[..]);
    }
}

#[test]
fn a_fit_board_without_a_boot_image_for_its_model_is_blocked() {
    let e = env_fit("a");
    let img = sealed("2.0.0", 64, &key());
    // Only an Android boot image, and a FIT for another model.
    let android = bootimg(0xcc, 8192);
    let other = fit(0xdd, 8192);
    let m = with_boots(&e, &img, Some(&android), &[(&["echo-9"], &other)]);
    let m = e.publish_json(&img, &m, Some(&key()));
    let blocked = check(&e.opts, m.to_str().unwrap()).unwrap().blocked;
    assert_eq!(blocked.as_ref().map(Blocked::kind), Some("boot_image"));
    assert_eq!(
        blocked.unwrap().to_string(),
        "This release has no boot image for the Echo Dot (5th gen): install a release built for it."
    );
    assert!(e.install(&m).is_err());
    assert!(e.part("boot_b").iter().all(|&x| x == 0xbb));
    assert!(e.part("system_b").iter().all(|&x| x == 0xbb));
    assert_eq!(e.record(), None);
}

#[test]
fn a_boot_image_of_the_wrong_format_is_refused_before_anything_is_written() {
    let img = sealed("2.0.0", 64, &key());
    // An Android image where the board reads FITs.
    let e = env_fit("a");
    let m = with_boots(&e, &img, None, &[(&["echo-dot-5"], &bootimg(0xcc, 8192))]);
    let m = e.publish_json(&img, &m, Some(&key()));
    let err = e.install(&m).unwrap_err();
    assert!(format!("{err:#}").contains("needs a FIT"), "{err:#}");
    assert!(e.part("boot_b").iter().all(|&x| x == 0xbb));
    assert!(e.part("system_b").iter().all(|&x| x == 0xbb));
    assert_eq!(e.record(), None);

    // A FIT where the board reads Android boot images.
    let e = env("a", None);
    e.with_boot_parts();
    let m = with_boots(&e, &img, None, &[(&["echo-dot-2"], &fit(0xdd, 8192))]);
    let m = e.publish_json(&img, &m, Some(&key()));
    let err = e.install(&m).unwrap_err();
    assert!(
        format!("{err:#}").contains("needs an Android boot image"),
        "{err:#}"
    );
}

#[test]
fn an_image_larger_than_the_boards_slot_is_refused() {
    let e = env_fit("a");
    let slot = pinna_board::Model::EchoDot5.board().slots.root;
    let big = std::fs::OpenOptions::new()
        .write(true)
        .open(e.path("parts/system_b"))
        .unwrap();
    big.set_len(slot + (8 << 20)).unwrap();
    let img = sealed("2.0.0", 64, &key());
    let itb = fit(0xdd, 8192);
    let mut m = with_boots(&e, &img, None, &[(&["echo-dot-5"], &itb)]);
    m["root"]["size"] = (slot + 1).into();
    let m = e.publish_json(&img, &m, Some(&key()));
    let err = e.install(&m).unwrap_err();
    assert!(
        format!("{err:#}").contains("does not fit into the Echo Dot (5th gen)'s root slot"),
        "{err:#}"
    );
}
