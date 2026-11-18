//! Playlist parsing for the media lane (ADR 0017).
//!
//! `.m3u` and `.pls` files name a stream or file to play. The parser is pure:
//! it takes the playlist body and the playlist's own URL, and returns the
//! first http(s) entry, resolving relative entries against that URL. Only one
//! level is ever followed: an entry that is itself a playlist is returned as
//! an address, not fetched. HLS segment playlists are recognised by their
//! `#EXT-X-` tags and refused with a sentence.

/// What an owner reads when a playlist is an HLS segment stream.
pub const HLS_SENTENCE: &str =
    "This is an HLS playlist, which doesn't play here. Use the stream's own address.";

/// Why a playlist can't be played.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistError {
    /// An HLS segment playlist; Pinna plays a single stream, not segments.
    Hls,
    /// No http(s) address in it.
    NoEntry,
}

impl PlaylistError {
    /// The owner-facing sentence for this error.
    pub const fn sentence(self) -> &'static str {
        match self {
            Self::Hls => HLS_SENTENCE,
            Self::NoEntry => "This playlist names no http:// or https:// address to play.",
        }
    }
}

impl std::fmt::Display for PlaylistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.sentence())
    }
}

impl std::error::Error for PlaylistError {}

/// The first playable address a playlist names, resolved against `base`
/// (the playlist's own URL).
///
/// Returns `Ok(None)` when the playlist is empty or names no http(s) address,
/// and `Err(PlaylistError::Hls)` for an HLS segment playlist.
pub fn parse(base: &str, body: &str) -> Result<String, PlaylistError> {
    if is_hls(body) {
        return Err(PlaylistError::Hls);
    }
    let entry = if looks_like_pls(body) {
        parse_pls(base, body)
    } else {
        parse_m3u(base, body)
    };
    entry.ok_or(PlaylistError::NoEntry)
}

/// A `.pls` file marks itself with a `[playlist]` section or a `File<n>` key.
fn looks_like_pls(body: &str) -> bool {
    body.lines().any(|line| {
        let line = line.trim();
        if line.eq_ignore_ascii_case("[playlist]") {
            return true;
        }
        match line.split_once('=') {
            Some((key, _)) => pls_index(key.trim()).is_some(),
            None => false,
        }
    })
}

/// The `<n>` in a `File<n>` key, if that is what the key is.
fn pls_index(key: &str) -> Option<u32> {
    let lower = key.to_ascii_lowercase();
    lower.strip_prefix("file")?.parse().ok()
}

/// `#EXT-X-` (case-insensitively) marks an HLS playlist.
fn is_hls(body: &str) -> bool {
    body.lines().any(|line| {
        line.trim_start()
            .get(..7)
            .is_some_and(|tag| tag.eq_ignore_ascii_case("#EXT-X-"))
    })
}

fn parse_m3u(base: &str, body: &str) -> Option<String> {
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(url) = resolve_entry(base, line) {
            return Some(url);
        }
    }
    None
}

fn parse_pls(base: &str, body: &str) -> Option<String> {
    let mut files: Vec<(u32, &str)> = Vec::new();
    for line in body.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        if let Some(index) = pls_index(key.trim()) {
            files.push((index, value.trim()));
        }
    }
    // Entries are ordered by their index, not their place in the file.
    files.sort_by_key(|(index, _)| *index);
    files
        .into_iter()
        .find_map(|(_, value)| resolve_entry(base, value))
}

/// Resolve one playlist entry to an http(s) URL, or `None` when it is neither
/// http(s) nor resolvable against an http(s) `base`.
fn resolve_entry(base: &str, entry: &str) -> Option<String> {
    let entry = entry.trim();
    if entry.is_empty() {
        return None;
    }
    if is_http(entry) {
        return Some(entry.to_string());
    }
    // A named scheme (mms:, ftp:, file:) is not http(s); never join it.
    if has_scheme(entry) {
        return None;
    }
    let (scheme, authority, path) = split_base(base)?;
    if let Some(rest) = entry.strip_prefix("//") {
        return Some(format!("{scheme}://{rest}"));
    }
    let joined = if entry.starts_with('/') {
        entry.to_string()
    } else {
        let dir = match path.rfind('/') {
            Some(i) => &path[..=i],
            None => "/",
        };
        format!("{dir}{entry}")
    };
    Some(format!("{scheme}://{authority}{}", normalize_path(&joined)))
}

/// The scheme, authority and (query-less) path of an http(s) URL.
fn split_base(base: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = base.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    Some((scheme, authority, path))
}

/// Collapse `.` and `..` segments and drop a query or fragment.
fn normalize_path(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    let mut out = String::from("/");
    out.push_str(&segments.join("/"));
    out
}

fn is_http(url: &str) -> bool {
    let head = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    head.starts_with("http://") || head.starts_with("https://")
}

/// Whether an entry starts with a URI scheme (`scheme:`) other than http(s).
fn has_scheme(entry: &str) -> bool {
    let Some((scheme, _)) = entry.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The entry, or `None` for a playlist that names none.
    fn resolved(base: &str, body: &str) -> Option<String> {
        match parse(base, body) {
            Ok(entry) => Some(entry),
            Err(PlaylistError::NoEntry) => None,
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn plain_m3u_names_its_first_entry() {
        let body = "http://radio.example/first.mp3\nhttp://radio.example/second.mp3\n";
        assert_eq!(
            resolved("http://radio.example/list.m3u", body).as_deref(),
            Some("http://radio.example/first.mp3")
        );
    }

    #[test]
    fn extended_m3u_skips_headers_and_metadata() {
        let body = "#EXTM3U\n#EXTINF:180,Artist - Title\n/dir/station.mp3\n";
        assert_eq!(
            resolved("http://radio.example/list.m3u", body).as_deref(),
            Some("http://radio.example/dir/station.mp3")
        );
    }

    #[test]
    fn m3u_skips_comments_and_blank_lines() {
        let body = "\n# a comment\n\nstation.mp3\n";
        assert_eq!(
            resolved("http://radio.example/radio/list.m3u", body).as_deref(),
            Some("http://radio.example/radio/station.mp3")
        );
    }

    #[test]
    fn relative_entries_resolve_against_the_playlist_url() {
        let base = "http://radio.example/radio/list.m3u";
        assert_eq!(
            resolved(base, "station.mp3\n").as_deref(),
            Some("http://radio.example/radio/station.mp3")
        );
        assert_eq!(
            resolved(base, "./station.mp3\n").as_deref(),
            Some("http://radio.example/radio/station.mp3")
        );
        assert_eq!(
            resolved(base, "../other.mp3\n").as_deref(),
            Some("http://radio.example/other.mp3")
        );
        assert_eq!(
            resolved(base, "/root.mp3\n").as_deref(),
            Some("http://radio.example/root.mp3")
        );
        assert_eq!(
            resolved(base, "//cdn.example/stream.mp3\n").as_deref(),
            Some("http://cdn.example/stream.mp3")
        );
    }

    #[test]
    fn relative_entry_uses_the_directory_when_the_base_has_an_authority_only() {
        assert_eq!(
            resolved("http://radio.example", "station.mp3\n").as_deref(),
            Some("http://radio.example/station.mp3")
        );
    }

    #[test]
    fn an_absolute_entry_is_returned_unchanged() {
        let body = "https://cdn.example/a.mp3\n";
        assert_eq!(
            resolved("http://radio.example/list.m3u", body).as_deref(),
            Some("https://cdn.example/a.mp3")
        );
    }

    #[test]
    fn the_first_http_entry_wins_over_other_schemes() {
        let body = "mms://old.example/a\nhttp://radio.example/b.mp3\n";
        assert_eq!(
            resolved("http://radio.example/list.m3u", body).as_deref(),
            Some("http://radio.example/b.mp3")
        );
    }

    #[test]
    fn relative_entries_need_an_http_base() {
        assert_eq!(resolved("file:///tmp/list.m3u", "station.mp3\n"), None);
    }

    #[test]
    fn a_playlist_with_no_entry_is_none() {
        assert_eq!(resolved("http://radio.example/list.m3u", "#EXTM3U\n"), None);
        assert_eq!(resolved("http://radio.example/list.m3u", ""), None);
        assert_eq!(
            resolved("http://radio.example/list.m3u", "mms://old.example/a\n"),
            None
        );
    }

    #[test]
    fn crlf_is_accepted() {
        let body = "#EXTM3U\r\n#EXTINF:1,One\r\nhttp://radio.example/a.mp3\r\n";
        assert_eq!(
            resolved("http://radio.example/list.m3u", body).as_deref(),
            Some("http://radio.example/a.mp3")
        );
    }

    #[test]
    fn pls_names_file_entries_in_index_order() {
        let body = "[playlist]\nFile2=http://radio.example/b.mp3\nFile1=http://radio.example/a.mp3\n\
                    NumberOfEntries=2\n";
        assert_eq!(
            resolved("http://radio.example/radio.pls", body).as_deref(),
            Some("http://radio.example/a.mp3")
        );
    }

    #[test]
    fn pls_resolves_relative_entries() {
        let body = "[playlist]\nFile1=station.mp3\nNumberOfEntries=1\n";
        assert_eq!(
            resolved("http://radio.example/radio/list.pls", body).as_deref(),
            Some("http://radio.example/radio/station.mp3")
        );
    }

    #[test]
    fn pls_keys_are_case_insensitive() {
        let body = "[playlist]\nfile1=http://radio.example/a.mp3\n";
        assert_eq!(
            resolved("http://radio.example/list.pls", body).as_deref(),
            Some("http://radio.example/a.mp3")
        );
    }

    #[test]
    fn a_pls_without_files_is_none() {
        assert_eq!(
            resolved(
                "http://radio.example/list.pls",
                "[playlist]\nNumberOfEntries=0\n"
            ),
            None
        );
    }

    #[test]
    fn hls_is_refused_with_a_sentence() {
        let body = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=128000\nlow.m3u8\n";
        assert_eq!(
            parse("http://radio.example/list.m3u8", body),
            Err(PlaylistError::Hls)
        );
        assert!(HLS_SENTENCE.contains("HLS playlist"));
        assert_eq!(PlaylistError::Hls.sentence(), HLS_SENTENCE);
    }

    #[test]
    fn hls_tags_are_case_insensitive() {
        let body = "#extm3u\n#ext-x-targetduration:10\n";
        assert_eq!(
            parse("http://radio.example/list.m3u8", body),
            Err(PlaylistError::Hls)
        );
    }
}
