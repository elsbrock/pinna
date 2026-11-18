//! The sentence splitter, the markdown stripper and the spoken-character cap
//! (ADR 0017 §"The reply streams").
//!
//! Model text arrives as deltas. [`Sentences`] buffers them and hands whole
//! sentences to text-to-speech, stripping markdown and links first because a
//! reply is spoken. A sentence is only complete once the next word has arrived,
//! so a stream fed one character at a time behaves exactly like a whole string.

/// Splits streamed text into spoken sentences, up to a character cap.
pub struct Sentences {
    buf: Vec<char>,
    cap: usize,
    spoken: usize,
    capped: bool,
}

impl Sentences {
    /// `cap` is the most characters that may be spoken in the reply.
    pub fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
            spoken: 0,
            capped: false,
        }
    }

    /// Add a delta and return every sentence that is now complete.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buf.extend(delta.chars());
        let mut out = Vec::new();
        while let Some(end) = sentence_end(&self.buf) {
            let raw: String = self.buf.drain(..end).collect();
            let spoken = speakable(&raw);
            if !spoken.is_empty() {
                let take = self.take(&spoken);
                if !take.is_empty() {
                    out.push(take);
                }
            }
        }
        out
    }

    /// Flush the text that has no terminator yet, at the end of the stream.
    pub fn finish(&mut self) -> Option<String> {
        let raw: String = std::mem::take(&mut self.buf).into_iter().collect();
        let spoken = speakable(&raw);
        if spoken.is_empty() {
            return None;
        }
        let take = self.take(&spoken);
        (!take.is_empty()).then_some(take)
    }

    /// True once the character cap has cut text off.
    pub fn capped(&self) -> bool {
        self.capped
    }

    fn take(&mut self, s: &str) -> String {
        if self.spoken >= self.cap {
            self.capped = true;
            return String::new();
        }
        let remaining = self.cap - self.spoken;
        let out: String = if s.chars().count() > remaining {
            self.capped = true;
            s.chars().take(remaining).collect()
        } else {
            s.to_string()
        };
        self.spoken += out.chars().count();
        out
    }
}

fn is_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…') || is_cjk_terminator(c)
}

/// Chinese and Japanese sentence ends, which aren't followed by a space.
fn is_cjk_terminator(c: char) -> bool {
    matches!(c, '。' | '｡' | '！' | '？')
}

/// Closing quotes and brackets that belong to the sentence before them.
fn is_closer(c: char) -> bool {
    matches!(c, '」' | '』' | '”' | '’' | '）' | ')' | '"')
}

/// The index just after a complete sentence, or `None` when more text is
/// needed. The trailing whitespace is left for the next sentence.
fn sentence_end(buf: &[char]) -> Option<usize> {
    let mut i = 0;
    while i < buf.len() {
        if !is_terminator(buf[i]) {
            i += 1;
            continue;
        }
        let mut j = i;
        let mut cjk = false;
        while j < buf.len() && is_terminator(buf[j]) {
            cjk |= is_cjk_terminator(buf[j]);
            j += 1;
        }
        if cjk {
            while j < buf.len() && is_closer(buf[j]) {
                j += 1;
            }
            // CJK text runs on without a space: once anything follows, the
            // sentence is complete.
            if j < buf.len() {
                return Some(j);
            }
            return None;
        }
        if j < buf.len() && buf[j].is_whitespace() {
            let mut k = j;
            while k < buf.len() && buf[k].is_whitespace() {
                k += 1;
            }
            if k == buf.len() {
                // The next word hasn't arrived: the boundary is undecided.
                return None;
            }
            if real_boundary(buf, i, k) {
                return Some(j);
            }
        }
        i = j;
    }
    None
}

/// Abbreviations (without the trailing dot) that don't end a sentence.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "e.g", "i.e", "nr", "ca",
    "approx", "incl", "excl", "fig", "dept", "univ", "inc", "ltd", "co", "corp", "kg", "gmbh",
    "bzw", "usw", "z.b", "u.a", "d.h", "bspw", "evtl", "ggf", "inkl", "exkl", "abb", "bzgl", "max",
    "min", "abs", "art", "bd", "sog", "vgl",
];

/// Whether the terminator run ending at `dot_start` really ends a sentence,
/// given the next non-whitespace character at `next`.
fn real_boundary(buf: &[char], dot_start: usize, next: usize) -> bool {
    let mut start = dot_start;
    while start > 0 && (buf[start - 1].is_alphanumeric() || buf[start - 1] == '.') {
        start -= 1;
    }
    let token = &buf[start..dot_start];
    if token.is_empty() {
        return true;
    }
    let lower: String = token.iter().collect::<String>().to_lowercase();
    if ABBREVIATIONS.contains(&lower.as_str()) {
        return false;
    }
    // A lone capital is an initial, as in "J. Smith".
    if token.len() == 1 && token[0].is_uppercase() {
        return false;
    }
    // A number at the start of a line is an ordered-list marker, and a number
    // before a lowercase word is an ordinal ("am 1. mai").
    let line_start = start == 0 || buf[start - 1] == '\n';
    if token.iter().all(char::is_ascii_digit) && (line_start || buf[next].is_lowercase()) {
        return false;
    }
    true
}

/// Strip markdown and links so the text can be spoken: labels of links stay,
/// URLs, emphasis markers, code ticks, headings and list markers go.
fn speakable(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut line_start = true;
    while i < chars.len() {
        let c = chars[i];

        if line_start {
            if matches!(c, '#' | '>') {
                while i < chars.len() && matches!(chars[i], '#' | '>' | ' ') {
                    i += 1;
                }
                line_start = false;
                continue;
            }
            if matches!(c, '-' | '+' | '*') && chars.get(i + 1) == Some(&' ') {
                i += 2;
                line_start = false;
                continue;
            }
            if c.is_ascii_digit() {
                let mut j = i;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                if matches!(chars.get(j), Some('.' | ')')) && chars.get(j + 1) == Some(&' ') {
                    i = j + 2;
                    line_start = false;
                    continue;
                }
            }
        }

        // A link keeps its label; an image's label too.
        if c == '!'
            && chars.get(i + 1) == Some(&'[')
            && let Some((label, end)) = parse_link(&chars, i + 1)
        {
            out.push_str(&label);
            i = end;
            line_start = false;
            continue;
        }
        if c == '['
            && let Some((label, end)) = parse_link(&chars, i)
        {
            out.push_str(&label);
            i = end;
            line_start = false;
            continue;
        }

        // Inline code and emphasis markers are not spoken.
        if matches!(c, '`' | '*' | '_') {
            i += 1;
            continue;
        }

        // A bare URL is a link, and links are not spoken.
        if (c == 'h' || c == 'H')
            && (starts_with_ci(&chars[i..], "http://") || starts_with_ci(&chars[i..], "https://"))
        {
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            continue;
        }

        if c == '\n' {
            out.push(' ');
            line_start = true;
            i += 1;
            continue;
        }
        if c == '\r' {
            i += 1;
            continue;
        }
        line_start = false;
        out.push(c);
        i += 1;
    }
    collapse_ws(&out)
}

/// `chars[start]` is `[`; return the label and the index after `)`.
fn parse_link(chars: &[char], start: usize) -> Option<(String, usize)> {
    let close = chars[start + 1..].iter().position(|&c| c == ']')? + start + 1;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = chars[close + 2..].iter().position(|&c| c == ')')? + close + 2;
    let label: String = chars[start + 1..close].iter().collect();
    Some((label, end + 1))
}

fn starts_with_ci(hay: &[char], needle: &str) -> bool {
    hay.len() >= needle.len()
        && hay[..needle.len()]
            .iter()
            .zip(needle.chars())
            .all(|(a, b)| a.eq_ignore_ascii_case(&b))
}

fn collapse_ws(s: &str) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(joined.len());
    for c in joined.chars() {
        if matches!(c, ',' | '.' | '!' | '?' | ';' | ':' | ')' | '%') && out.ends_with(' ') {
            out.pop();
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(text: &str) -> Vec<String> {
        let mut s = Sentences::new(1_000);
        let mut out = s.push(text);
        if let Some(last) = s.finish() {
            out.push(last);
        }
        out
    }

    #[test]
    fn english_sentences() {
        assert_eq!(
            collect("Hello there. How are you? I am fine!"),
            ["Hello there.", "How are you?", "I am fine!"]
        );
    }

    #[test]
    fn german_sentences() {
        assert_eq!(
            collect("Hallo. Wie geht es dir? Mir geht es gut!"),
            ["Hallo.", "Wie geht es dir?", "Mir geht es gut!"]
        );
    }

    #[test]
    fn chinese_and_japanese_sentences_split_without_spaces() {
        assert_eq!(
            collect("你好。今天天气很好！"),
            ["你好。", "今天天气很好！"]
        );
        assert_eq!(
            collect("「はい。」と言った。"),
            ["「はい。」", "と言った。"]
        );
        // A sentence end at the very end waits for what follows, or the finish.
        let mut s = Sentences::new(1_000);
        assert!(s.push("你好。").is_empty());
        assert_eq!(s.push("再见"), ["你好。"]);
    }

    #[test]
    fn abbreviations_do_not_split() {
        assert_eq!(
            collect("Use one, e.g. this. Then stop."),
            ["Use one, e.g. this.", "Then stop."]
        );
        assert_eq!(
            collect("Dr. Smith is here. Hallo."),
            ["Dr. Smith is here.", "Hallo."]
        );
        assert_eq!(
            collect("Das ist z.B. gut. Ja."),
            ["Das ist z.B. gut.", "Ja."]
        );
        // An initial is not a sentence.
        assert_eq!(collect("J. Smith."), ["J. Smith."]);
    }

    #[test]
    fn numbers_do_not_split() {
        assert_eq!(
            collect("It costs 3.14 euros. That is a lot."),
            ["It costs 3.14 euros.", "That is a lot."]
        );
        assert_eq!(
            collect("Es kostet 1.000 euro. Ja."),
            ["Es kostet 1.000 euro.", "Ja."]
        );
        // A sentence that ends in a number still splits.
        assert_eq!(collect("Version 2. It works."), ["Version 2.", "It works."]);
    }

    #[test]
    fn markdown_is_stripped() {
        assert_eq!(collect("**Bold** and `code`."), ["Bold and code."]);
        assert_eq!(
            collect("# Heading\n- first\n- second."),
            ["Heading first second."]
        );
        assert_eq!(collect("1. First\n2. Second."), ["First Second."]);
        assert_eq!(collect("_Italic_ and *also*."), ["Italic and also."]);
    }

    #[test]
    fn links_are_stripped() {
        assert_eq!(
            collect("See [the docs](https://example.com/x) now."),
            ["See the docs now."]
        );
        assert_eq!(collect("Visit https://example.com now."), ["Visit now."]);
    }

    #[test]
    fn a_stream_matches_a_whole_string() {
        let text = "Hello there. Use one, e.g. this. It costs 3.14 euros. Bye!";
        let whole = collect(text);
        let mut s = Sentences::new(1_000);
        let mut streamed = Vec::new();
        for c in text.chars() {
            streamed.extend(s.push(&c.to_string()));
        }
        if let Some(last) = s.finish() {
            streamed.push(last);
        }
        assert_eq!(whole, streamed);
    }

    #[test]
    fn a_sentence_waits_for_the_next_word() {
        let mut s = Sentences::new(1_000);
        assert!(s.push("Hello. ").is_empty());
        assert_eq!(s.push("How"), ["Hello."]);
        assert_eq!(s.finish().unwrap(), "How");
    }

    #[test]
    fn finish_flushes_without_a_terminator() {
        assert_eq!(collect("no terminator here"), ["no terminator here"]);
    }

    #[test]
    fn the_cap_limits_spoken_text() {
        let mut s = Sentences::new(10);
        // The first sentence is longer than the cap: it is cut, and the rest
        // is dropped.
        let first = s.push("Hello world. More text.");
        assert_eq!(first, ["Hello worl"]);
        assert!(s.capped());
        assert!(s.finish().is_none());
    }

    #[test]
    fn the_cap_counts_sentences() {
        let mut s = Sentences::new(20);
        assert!(s.push("Hi. ").is_empty());
        assert_eq!(s.push("Bye"), ["Hi."]);
        assert!(s.push(" now.").is_empty());
        assert_eq!(s.finish().unwrap(), "Bye now.");
        assert!(!s.capped());

        let mut small = Sentences::new(6);
        assert!(small.push("Hi. ").is_empty());
        assert_eq!(small.push("Bye"), ["Hi."]);
        assert!(small.push(" now.").is_empty());
        // Only three characters are left.
        assert_eq!(small.finish().unwrap(), "Bye");
        assert!(small.capped());
    }
}
