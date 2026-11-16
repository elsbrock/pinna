//! shairport-sync's metadata pipe: a text stream of `<item>` elements.

use base64::Engine as _;

/// One metadata item: four-character type and code, and the decoded data.
#[derive(Debug, PartialEq, Eq)]
pub struct Item {
    pub kind: String,
    pub code: String,
    pub data: Vec<u8>,
}

/// More than any real item (cover art is off); a stream without a closing tag is dropped.
const MAX_PENDING: usize = 1 << 20;

#[derive(Default)]
pub struct Parser {
    buf: String,
}

impl Parser {
    /// Feeds more text; returns the items completed by it.
    pub fn push(&mut self, text: &str) -> Vec<Item> {
        self.buf.push_str(text);
        let mut items = Vec::new();
        loop {
            let Some(start) = self.buf.find("<item>") else {
                // Keep a possible partial "<item>" at the end.
                let keep = self.buf.len().min(5);
                let mut cut = self.buf.len() - keep;
                while !self.buf.is_char_boundary(cut) {
                    cut += 1;
                }
                self.buf.drain(..cut);
                break;
            };
            self.buf.drain(..start);
            let Some(end) = self.buf.find("</item>") else {
                if self.buf.len() > MAX_PENDING {
                    self.buf.clear();
                }
                break;
            };
            let body: String = self.buf.drain(..end + "</item>".len()).collect();
            if let Some(item) = parse_item(&body) {
                items.push(item);
            }
        }
        items
    }
}

fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let from = s.find(&open)? + open.len();
    let to = s[from..].find(&format!("</{name}>"))? + from;
    Some(&s[from..to])
}

fn hex_text(s: &str) -> Option<String> {
    if s.len() != 8 {
        return None;
    }
    let bytes = (0..4)
        .map(|i| u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

fn parse_item(body: &str) -> Option<Item> {
    let kind = hex_text(tag(body, "type")?.trim())?;
    let code = hex_text(tag(body, "code")?.trim())?;
    let data = match body.find("<data") {
        Some(at) => {
            let rest = &body[at..];
            let from = rest.find('>')? + 1;
            let to = rest.find("</data>")?;
            let text: String = rest.get(from..to)?.split_whitespace().collect();
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .ok()?
        }
        None => Vec::new(),
    };
    Some(Item { kind, code, data })
}

/// Maps an AirPlay volume (dB, -30..=0, -144 for mute) to a percentage.
/// Only mute is 0: -30 is the sender's quietest step, still audible.
pub fn volume_percent(airplay_volume: f64) -> u8 {
    if airplay_volume <= -144.0 {
        return 0;
    }
    (1.0 + ((airplay_volume + 30.0) / 30.0 * 99.0).round()).clamp(1.0, 100.0) as u8
}

/// The percentage of an `ssnc`/`pvol` item's `airplay_volume,volume,lowest,highest` data.
pub fn pvol_percent(data: &[u8]) -> Option<u8> {
    let text = std::str::from_utf8(data).ok()?;
    let av: f64 = text.split(',').next()?.trim().parse().ok()?;
    av.is_finite().then(|| volume_percent(av))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PVOL: &str = "<item><type>73736e63</type><code>70766f6c</code><length>27</length>\n<data encoding=\"base64\">\nLTI0LjQ5LC0yNC40OSwtOTYuMzAsMC4wMA==</data></item>";

    fn pvol_item() -> Item {
        Item {
            kind: "ssnc".into(),
            code: "pvol".into(),
            data: b"-24.49,-24.49,-96.30,0.00".to_vec(),
        }
    }

    #[test]
    fn full_pvol_item() {
        let items = Parser::default().push(PVOL);
        assert_eq!(items, vec![pvol_item()]);
        assert_eq!(pvol_percent(&items[0].data), Some(19));
    }

    #[test]
    fn item_split_across_pushes() {
        let mut p = Parser::default();
        let (a, b) = PVOL.split_at(70);
        assert!(p.push(a).is_empty());
        assert_eq!(p.push(b), vec![pvol_item()]);
    }

    #[test]
    fn zero_length_item() {
        let items = Parser::default()
            .push("<item><type>73736e63</type><code>70666c73</code><length>0</length></item>\n");
        assert_eq!(
            items,
            vec![Item {
                kind: "ssnc".into(),
                code: "pfls".into(),
                data: Vec::new()
            }]
        );
    }

    #[test]
    fn garbage_between_items() {
        let text = format!("junk <it{PVOL}\n\u{fffd}<<>> {PVOL}tail<ite");
        let mut p = Parser::default();
        assert_eq!(p.push(&text).len(), 2);
        assert_eq!(p.push("m>"), vec![]);
    }

    #[test]
    fn multiple_items_in_one_push() {
        let text = format!(
            "{PVOL}\n<item><type>73736e63</type><code>70656e64</code><length>0</length></item>"
        );
        let items = Parser::default().push(&text);
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].code, "pend");
    }

    #[test]
    fn volume_mapping() {
        assert_eq!(volume_percent(-144.0), 0);
        assert_eq!(volume_percent(-200.0), 0);
        assert_eq!(volume_percent(-30.0), 1, "quietest, not mute");
        assert_eq!(volume_percent(-15.0), 51);
        assert_eq!(volume_percent(0.0), 100);
        assert_eq!(volume_percent(5.0), 100);
        assert_eq!(volume_percent(-60.0), 1);
    }
}
