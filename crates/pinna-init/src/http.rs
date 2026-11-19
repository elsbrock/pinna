//! The rescue page on <http://10.42.0.1> (ADR 0012), reachable only over the
//! USB network: why the device is in rescue mode, pinna-init's log, one
//! download with every log, and a restart.
//!
//! Every request must name `10.42.0.1` as its Host (no DNS rebinding), and a
//! POST must come from this page (an `Origin` other than `http://10.42.0.1`
//! is refused), so a website open in the computer's browser can neither read
//! the logs nor restart the device.

use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use crate::rescue::{ADDRESS, DOCS, FASTBOOT, Info, reboot_now};

const PSTORE: &str = "/sys/fs/pstore";
const ISSUES: &str = "https://github.com/elsbrock/pinna/issues";
const MAX_HEAD: usize = 16 * 1024;

/// Accept connections on 10.42.0.1:80 for good, once the address exists.
pub fn serve(info: &Info) {
    let listener = loop {
        match TcpListener::bind((ADDRESS, 80)) {
            Ok(l) => break l,
            Err(_) => sleep(Duration::from_secs(1)),
        }
    };
    for stream in listener.incoming().flatten() {
        if handle(stream, info) {
            sleep(Duration::from_millis(500));
            reboot_now();
        }
    }
}

/// Answer one request; true when the device should restart now.
fn handle(mut s: TcpStream, info: &Info) -> bool {
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(10)));
    let mut head = Vec::new();
    let mut buf = [0u8; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < MAX_HEAD {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let head = String::from_utf8_lossy(&head);
    let resp = match parse(&head) {
        Some(req) => respond(&req, info, &Sources::default()),
        None => Response::text(400, "Bad Request", "bad request\n".into()),
    };
    let _ = s.write_all(&resp.bytes());
    let _ = s.flush();
    resp.restart
}

#[derive(Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub host: Option<&'a str>,
    pub origin: Option<&'a str>,
}

pub fn parse(head: &str) -> Option<Request<'_>> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?, first.next()?);
    first.next()?.strip_prefix("HTTP/")?;
    let mut req = Request {
        method,
        path,
        host: None,
        origin: None,
    };
    for l in lines.take_while(|l| !l.is_empty()) {
        let (k, v) = l.split_once(':')?;
        if k.eq_ignore_ascii_case("host") {
            req.host = Some(v.trim());
        } else if k.eq_ignore_ascii_case("origin") {
            req.origin = Some(v.trim());
        }
    }
    Some(req)
}

pub struct Response {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    extra: Vec<String>,
    body: String,
    restart: bool,
}

impl Response {
    fn text(status: u16, reason: &'static str, body: String) -> Self {
        Response {
            status,
            reason,
            content_type: "text/plain; charset=utf-8",
            extra: Vec::new(),
            body,
            restart: false,
        }
    }

    fn html(body: String) -> Self {
        Response {
            content_type: "text/html; charset=utf-8",
            ..Response::text(200, "OK", body)
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut h = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n",
            self.status,
            self.reason,
            self.content_type,
            self.body.len()
        );
        for e in &self.extra {
            h.push_str(e);
            h.push_str("\r\n");
        }
        h.push_str("\r\n");
        let mut out = h.into_bytes();
        out.extend_from_slice(self.body.as_bytes());
        out
    }
}

fn host_ok(host: Option<&str>) -> bool {
    matches!(host, Some(h) if h == ADDRESS || h == format!("{ADDRESS}:80"))
}

fn origin_ok(origin: Option<&str>) -> bool {
    match origin {
        None => true,
        Some(o) => o == format!("http://{ADDRESS}"),
    }
}

/// Where the diagnostics come from (tests point these elsewhere).
pub struct Sources<'a> {
    pub pstore: &'a Path,
    pub partitions: &'a Path,
}

impl Default for Sources<'_> {
    fn default() -> Self {
        Sources {
            pstore: Path::new(PSTORE),
            partitions: Path::new("/proc/partitions"),
        }
    }
}

pub fn respond(req: &Request, info: &Info, src: &Sources) -> Response {
    if !host_ok(req.host) {
        return Response::text(
            421,
            "Misdirected Request",
            format!("use http://{ADDRESS}\n"),
        );
    }
    match (req.method, req.path) {
        ("GET", "/") => Response::html(page(info)),
        ("GET", "/diagnostics.txt") => {
            let mut r = Response::text(200, "OK", diagnostics(info, src));
            r.extra.push(
                "Content-Disposition: attachment; filename=\"pinna-rescue-diagnostics.txt\"".into(),
            );
            r
        }
        ("POST", "/restart") if origin_ok(req.origin) => {
            crate::log!("rescue: restart requested on the page");
            let mut r = Response::html(restarting());
            r.restart = true;
            r
        }
        ("POST", "/restart") => Response::text(403, "Forbidden", "cross-site request\n".into()),
        (_, "/" | "/diagnostics.txt" | "/restart") => {
            Response::text(405, "Method Not Allowed", "method not allowed\n".into())
        }
        _ => Response::text(404, "Not Found", "not found\n".into()),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const STYLE: &str = "body{font:16px/1.5 system-ui,sans-serif;max-width:46rem;margin:2rem auto;\
padding:0 1rem;color:#1b1b1b;background:#fff}pre{white-space:pre-wrap;word-break:break-word;\
background:#f2f2f2;padding:.75rem;font-size:13px}button{font:inherit;padding:.4rem 1rem}\
@media (prefers-color-scheme:dark){body{color:#e6e6e6;background:#161616}pre{background:#262626}\
a{color:#8ab4ff}}";

fn page(info: &Info) -> String {
    let log = crate::log_lines().join("\n");
    format!(
        "<!doctype html><html lang=en><meta charset=utf-8>\
<meta name=viewport content=\"width=device-width,initial-scale=1\">\
<title>Pinna rescue</title><style>{STYLE}</style>\
<h1>Pinna rescue mode</h1>\
<p>{explain}</p><pre>{reason}: {detail}</pre>\
<h2>To fix it</h2><p>{fix} {FASTBOOT} See <a href=\"{DOCS}\">Rescue mode</a> in the docs.</p>\
<p><a href=/diagnostics.txt download>Download diagnostics</a>: pinna-init's log, the kernel \
log, the slot records, the previous boot's records and the partitions, in one text file. To \
report the problem, <a href=\"{ISSUES}\">open an issue</a> and attach it. Issues are public: \
read the file first. It holds the device's serial number and the kernel log.</p>\
<form method=post action=/restart><button>Restart</button> boots the same way, and ends here \
again unless something changed. Download the diagnostics first: a restart replaces the previous \
boot's records.</form>\
<p>A root shell is on the USB serial port: <code>picocom -b 115200 /dev/ttyACM0</code>.</p>\
<h2>pinna-init's log</h2><pre>{log}</pre>",
        explain = escape(info.reason.explain()),
        reason = info.reason,
        detail = escape(&info.detail),
        fix = escape(info.reason.fix(&info.detail)),
        log = escape(&log),
    )
}

fn restarting() -> String {
    format!(
        "<!doctype html><html lang=en><meta charset=utf-8>\
<meta http-equiv=refresh content=\"20;url=/\"><title>Pinna rescue</title>\
<style>{STYLE}</style><h1>Restarting</h1><p>The device restarts now. If the ring turns blue, \
Pinna started normally. If it is in rescue mode again, <a href=/>the rescue page</a> comes \
back here in 20 seconds and says why.</p>"
    )
}

/// The kernel log, as `dmesg` prints it.
fn dmesg() -> String {
    // SAFETY: klogctl with a valid buffer.
    let size = unsafe { libc::klogctl(10, std::ptr::null_mut(), 0) };
    let mut buf = vec![0u8; size.max(1 << 17) as usize];
    // SAFETY: as above; READ_ALL does not clear the log.
    let n = unsafe { libc::klogctl(3, buf.as_mut_ptr().cast(), buf.len() as libc::c_int) };
    if n <= 0 {
        return "(kernel log unavailable)\n".into();
    }
    String::from_utf8_lossy(&buf[..n as usize])
        .lines()
        .map(|l| match l.strip_prefix('<') {
            Some(rest) => rest.split_once('>').map_or(l, |(_, m)| m),
            None => l,
        })
        .fold(String::new(), |mut s, l| {
            s.push_str(l);
            s.push('\n');
            s
        })
}

pub fn diagnostics(info: &Info, src: &Sources) -> String {
    let mut out = String::new();
    let section = |out: &mut String, title: &str| {
        let _ = write!(out, "\n===== {title} =====\n");
    };
    let _ = writeln!(out, "Pinna rescue diagnostics");
    let _ = writeln!(out, "reason: {}", info.reason);
    let _ = writeln!(out, "detail: {}", info.detail);
    let _ = writeln!(out, "pinna-init: {}", pinna_version::VERSION);
    // Which key this boot image trusts: a root signed with another one
    // doesn't boot.
    let _ = writeln!(
        out,
        "release key: {}",
        pinna_image::keys::hex(&pinna_image::keys::RELEASE_PUBLIC_KEY)
    );
    if let Ok(release) = fs::read_to_string("/proc/sys/kernel/osrelease") {
        let _ = write!(out, "kernel: {release}");
    }
    if let Ok(up) = fs::read_to_string("/proc/uptime") {
        let _ = writeln!(out, "uptime: {} s", up.split(' ').next().unwrap_or("?"));
    }
    section(&mut out, "pinna-init log");
    for l in crate::log_lines() {
        out.push_str(&l);
        out.push('\n');
    }
    section(&mut out, "partitions");
    out.push_str(&fs::read_to_string(src.partitions).unwrap_or_else(|e| format!("({e})\n")));
    section(
        &mut out,
        "slot records in misc (the images are checked in the log above)",
    );
    out.push_str(&crate::slot_summary());
    section(&mut out, "previous boot (pstore)");
    let mut names: Vec<_> = fs::read_dir(src.pstore)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    names.sort();
    if names.is_empty() {
        out.push_str(
            "(no records: the previous boot ended by a power cut, or this is a cold start)\n",
        );
    }
    for p in names {
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        let _ = writeln!(out, "--- {name}");
        out.push_str(&String::from_utf8_lossy(&fs::read(&p).unwrap_or_default()));
        out.push('\n');
    }
    section(&mut out, "kernel log");
    out.push_str(&dmesg());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rescue::Reason;

    fn info() -> Info {
        Info {
            reason: Reason::NoRoot,
            detail: "slot a: <bad> signature".into(),
        }
    }

    fn req<'a>(
        method: &'a str,
        path: &'a str,
        host: &'a str,
        origin: Option<&'a str>,
    ) -> Request<'a> {
        Request {
            method,
            path,
            host: Some(host),
            origin,
        }
    }

    fn status(r: &Response) -> u16 {
        r.status
    }

    #[test]
    fn parses_request_head() {
        let r =
            parse("POST /restart HTTP/1.1\r\nHost: 10.42.0.1\r\norigin: http://10.42.0.1\r\n\r\n")
                .unwrap();
        assert_eq!(
            r,
            req("POST", "/restart", "10.42.0.1", Some("http://10.42.0.1"))
        );
        assert!(parse("garbage").is_none());
        assert!(parse("GET / FTP/1\r\n\r\n").is_none());
    }

    #[test]
    fn only_our_own_address_as_host() {
        let d = tempfile::tempdir().unwrap();
        let src = Sources {
            pstore: d.path(),
            partitions: d.path(),
        };
        let i = info();
        assert_eq!(
            status(&respond(&req("GET", "/", "10.42.0.1", None), &i, &src)),
            200
        );
        assert_eq!(
            status(&respond(&req("GET", "/", "10.42.0.1:80", None), &i, &src)),
            200
        );
        assert_eq!(
            status(&respond(&req("GET", "/", "evil.example", None), &i, &src)),
            421
        );
        let no_host = Request {
            host: None,
            ..req("GET", "/", "", None)
        };
        assert_eq!(status(&respond(&no_host, &i, &src)), 421);
    }

    #[test]
    fn restart_only_from_this_page() {
        let d = tempfile::tempdir().unwrap();
        let src = Sources {
            pstore: d.path(),
            partitions: d.path(),
        };
        let i = info();
        let ok = respond(
            &req("POST", "/restart", "10.42.0.1", Some("http://10.42.0.1")),
            &i,
            &src,
        );
        assert!(ok.restart && ok.status == 200);
        let curl = respond(&req("POST", "/restart", "10.42.0.1", None), &i, &src);
        assert!(curl.restart);
        let cross = respond(
            &req(
                "POST",
                "/restart",
                "10.42.0.1",
                Some("https://evil.example"),
            ),
            &i,
            &src,
        );
        assert!(!cross.restart && cross.status == 403);
        let null = respond(
            &req("POST", "/restart", "10.42.0.1", Some("null")),
            &i,
            &src,
        );
        assert!(!null.restart);
        assert_eq!(
            status(&respond(
                &req("GET", "/restart", "10.42.0.1", None),
                &i,
                &src
            )),
            405
        );
        assert_eq!(
            status(&respond(&req("GET", "/x", "10.42.0.1", None), &i, &src)),
            404
        );
    }

    #[test]
    fn page_escapes_the_detail() {
        let p = page(&info());
        assert!(p.contains("slot a: &lt;bad&gt; signature"));
        assert!(!p.contains("<bad>"));
        assert!(p.contains("action=/restart"));
        assert!(p.contains("Volume Down"));
        assert!(p.contains("#rescue-mode"));
        let r = restarting();
        assert!(r.contains("content=\"20;url=/\""));
    }

    #[test]
    fn diagnostics_include_pstore_records() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("pmsg-ramoops-0"), "pinna: reboot (update)\n").unwrap();
        fs::write(d.path().join("partitions"), "major minor  #blocks  name\n").unwrap();
        let src = Sources {
            pstore: d.path(),
            partitions: &d.path().join("partitions"),
        };
        let t = diagnostics(&info(), &src);
        assert!(t.contains("reason: no-root"));
        assert!(t.contains("--- pmsg-ramoops-0\npinna: reboot (update)"));
        assert!(t.contains("major minor"));
        let empty = tempfile::tempdir().unwrap();
        let src = Sources {
            pstore: empty.path(),
            partitions: empty.path(),
        };
        assert!(diagnostics(&info(), &src).contains("(no records:"));
    }

    #[test]
    fn response_head() {
        let r = Response::text(404, "Not Found", "x\n".into());
        let b = String::from_utf8(r.bytes()).unwrap();
        assert!(b.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(b.contains("Content-Length: 2\r\n"));
        assert!(b.ends_with("\r\n\r\nx\n"));
    }
}
