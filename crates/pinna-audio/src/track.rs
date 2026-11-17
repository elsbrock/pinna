//! What the media lane's music is: the title, artist and album its source
//! names, and an internet radio station's name (ADR 0011). Home Assistant's
//! media player can't carry them; the dashboard shows them.
//!
//! A URL names its track in its tags (ID3v2, Vorbis comments, FLAC, RIFF
//! INFO) or, for internet radio, in ICY metadata: blocks the server
//! interleaves with the audio every `icy-metaint` bytes when asked with
//! `Icy-MetaData: 1`. A stream bridge names its own through the control API.

use symphonia::core::meta::{MetadataRevision, StandardTagKey};

/// The longest field kept, in bytes: enough for any real title, and a
/// bound on what an untrusted server or sender can make the device keep.
pub const FIELD_MAX: usize = 256;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// An internet radio station's name (`icy-name`).
    pub station: String,
}

impl Track {
    /// Each field trimmed and cut to [`FIELD_MAX`] bytes at a character
    /// boundary.
    pub fn capped(mut self) -> Track {
        for f in [
            &mut self.title,
            &mut self.artist,
            &mut self.album,
            &mut self.station,
        ] {
            *f = cap(f);
        }
        self
    }

    /// Fill the title, artist and album from a revision of tags; tags it
    /// lacks leave their field as it is.
    pub(crate) fn add_tags(&mut self, rev: &MetadataRevision) {
        for tag in rev.tags() {
            let field = match tag.std_key {
                Some(StandardTagKey::TrackTitle) => &mut self.title,
                Some(StandardTagKey::Artist) => &mut self.artist,
                Some(StandardTagKey::Album) => &mut self.album,
                _ => continue,
            };
            let value = cap(&tag.value.to_string());
            if !value.is_empty() {
                *field = value;
            }
        }
    }
}

fn cap(s: &str) -> String {
    let s = s.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    let mut end = s.len().min(FIELD_MAX);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].trim_end().to_string()
}

/// Text from a server that doesn't say its encoding: UTF-8 when it is valid,
/// otherwise Latin-1, which ICY servers commonly send.
pub(crate) fn text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

/// The `StreamTitle` of an ICY metadata block, as sent: stations put the
/// artist and title in it in no fixed order, so it isn't split. `None` when
/// the block has none; `Some("")` when the station cleared it.
pub(crate) fn stream_title(block: &[u8]) -> Option<String> {
    const KEY: &str = "StreamTitle='";
    let s = text(block);
    let from = s.find(KEY)? + KEY.len();
    let rest = &s[from..];
    // A title may itself hold a quote: the value ends at the quote that
    // ends the field, or at the end of the block.
    let to = rest
        .find("';")
        .unwrap_or_else(|| rest.trim_end_matches('\0').trim_end_matches('\'').len());
    Some(cap(&rest[..to]))
}

/// Splits an ICY body into audio and the metadata blocks interleaved in it:
/// after every `metaint` bytes of audio, one length byte `n`, then `16 * n`
/// bytes of text (`n` = 0: no change).
pub(crate) struct IcyDemux {
    metaint: usize,
    state: Icy,
    block: Vec<u8>,
}

enum Icy {
    /// Audio bytes before the next length byte.
    Audio(usize),
    Length,
    /// Metadata bytes still to come.
    Meta(usize),
}

impl IcyDemux {
    pub(crate) fn new(metaint: usize) -> IcyDemux {
        IcyDemux {
            metaint,
            state: Icy::Audio(metaint),
            block: Vec::new(),
        }
    }

    /// Append the audio in `data` to `audio`; return the last metadata block
    /// that completed in it, if any.
    pub(crate) fn feed(&mut self, mut data: &[u8], audio: &mut Vec<u8>) -> Option<Vec<u8>> {
        let mut done = None;
        while !data.is_empty() {
            match self.state {
                Icy::Audio(left) => {
                    let n = left.min(data.len());
                    audio.extend_from_slice(&data[..n]);
                    data = &data[n..];
                    self.state = if n == left {
                        Icy::Length
                    } else {
                        Icy::Audio(left - n)
                    };
                }
                Icy::Length => {
                    let len = usize::from(data[0]) * 16;
                    data = &data[1..];
                    self.state = if len == 0 {
                        Icy::Audio(self.metaint)
                    } else {
                        self.block.clear();
                        Icy::Meta(len)
                    };
                }
                Icy::Meta(left) => {
                    let n = left.min(data.len());
                    self.block.extend_from_slice(&data[..n]);
                    data = &data[n..];
                    if n == left {
                        done = Some(std::mem::take(&mut self.block));
                        self.state = Icy::Audio(self.metaint);
                    } else {
                        self.state = Icy::Meta(left - n);
                    }
                }
            }
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `metaint` bytes of audio per block, each block's metadata padded to
    /// 16 bytes, as a server sends it.
    fn icy_body(metaint: usize, parts: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (audio, meta) in parts {
            assert_eq!(audio.len(), metaint);
            out.extend_from_slice(audio);
            let n = meta.len().div_ceil(16);
            out.push(n as u8);
            out.extend_from_slice(meta);
            out.resize(out.len() + n * 16 - meta.len(), 0);
        }
        out
    }

    #[test]
    fn demux_returns_audio_only_and_the_titles() {
        let body = icy_body(
            8,
            &[
                (b"AAAAAAAA", b"StreamTitle='One';"),
                (b"BBBBBBBB", b""),
                (b"CCCCCCCC", b"StreamTitle='Two';StreamUrl='';"),
            ],
        );
        // Every split of the body gives the same audio; one small enough
        // gives each title, and any gives the last one.
        for step in [1, 3, 7, 16, body.len()] {
            let mut d = IcyDemux::new(8);
            let mut audio = Vec::new();
            let mut titles = Vec::new();
            for chunk in body.chunks(step) {
                if let Some(block) = d.feed(chunk, &mut audio) {
                    titles.push(stream_title(&block).unwrap());
                }
            }
            assert_eq!(audio, b"AAAAAAAABBBBBBBBCCCCCCCC", "step {step}");
            if step <= 16 {
                assert_eq!(titles, ["One", "Two"], "step {step}");
            } else {
                assert_eq!(titles, ["Two"], "step {step}");
            }
        }
    }

    #[test]
    fn stream_title_as_sent() {
        assert_eq!(
            stream_title(b"StreamTitle='Artist - Song';StreamUrl='x';\0\0").as_deref(),
            Some("Artist - Song")
        );
        assert_eq!(
            stream_title(b"StreamTitle='Don't Stop';\0").as_deref(),
            Some("Don't Stop")
        );
        assert_eq!(stream_title(b"StreamTitle='';\0\0").as_deref(), Some(""));
        assert_eq!(
            stream_title(b"StreamTitle='Cut short\0\0").as_deref(),
            Some("Cut short")
        );
        assert_eq!(stream_title(b"StreamUrl='x';"), None);
    }

    #[test]
    fn latin1_when_not_utf8() {
        assert_eq!(
            stream_title(b"StreamTitle='Caf\xe9 del Mar';").as_deref(),
            Some("Café del Mar")
        );
        assert_eq!(text("Café".as_bytes()), "Café");
    }

    #[test]
    fn capped_trims_and_cuts_at_a_character() {
        let t = Track {
            title: format!("  {}  ", "é".repeat(200)),
            artist: "\0Artist\0".into(),
            ..Default::default()
        }
        .capped();
        assert_eq!(t.title.len(), FIELD_MAX);
        assert!(t.title.chars().all(|c| c == 'é'));
        assert_eq!(t.artist, "Artist");
    }
}
