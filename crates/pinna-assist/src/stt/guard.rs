//! Drops the phrases speech-to-text invents on silence (ADR 0017 §"Errors").
//!
//! Whisper and the servers that copy its audio API answer a silent or musical
//! clip with a small set of stock phrases. Nothing was said, so the transcript
//! must not reach the brain. The match ignores case and punctuation and must
//! cover the whole transcript, so ordinary speech that happens to contain one
//! of the phrases is kept.

/// The phrases that mean nothing was heard. They are stored already normalised:
/// lowercase, without punctuation, one space between words.
const INVENTED: &[&str] = &[
    "thank you",
    "thanks for watching",
    "thank you for watching",
    "please subscribe",
    "subscribe to my channel",
    "subtitles by the amara org community",
    "amara org",
    "you",
    "bye",
    "okay",
    "the end",
    "danke",
    "danke schön",
    "danke schoen",
    "vielen dank",
    "untertitel",
    "untertitelung",
];

/// Whether `text` is only a phrase speech-to-text invents. An empty or
/// punctuation-only transcript also counts: nothing was heard.
pub fn is_invented(text: &str) -> bool {
    let normal = normalise(text);
    normal.is_empty() || INVENTED.contains(&normal.as_str())
}

/// Lowercase, replace every non-alphanumeric character with a space, and
/// collapse runs of spaces.
fn normalise(text: &str) -> String {
    let mapped: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_normalised() {
        for phrase in INVENTED {
            assert_eq!(*phrase, normalise(phrase), "{phrase} is not normalised");
            assert!(is_invented(phrase), "{phrase}");
        }
    }

    #[test]
    fn known_hallucinations_are_rejected() {
        for phrase in [
            "Thank you.",
            "thanks  for watching!",
            "THANK YOU FOR WATCHING",
            "Please subscribe.",
            "Subscribe to my channel.",
            "Subtitles by the Amara.org community",
            "Amara.org",
            "you",
            "Bye!",
            "Okay.",
            "The end.",
            "Danke.",
            "Danke schön!",
            "Vielen Dank.",
            "Untertitel",
        ] {
            assert!(is_invented(phrase), "{phrase}");
        }
    }

    #[test]
    fn real_speech_is_kept() {
        for text in [
            "Turn on the kitchen light",
            "What is the weather tomorrow?",
            "thank you for the reminder about the meeting",
            "Danke, schalte das Licht ein",
            "play some music",
            "Set a timer for five minutes",
        ] {
            assert!(!is_invented(text), "{text}");
        }
    }

    #[test]
    fn nothing_heard_counts_as_invented() {
        assert!(is_invented(""));
        assert!(is_invented("   "));
        assert!(is_invented("..."));
        assert!(is_invented("\n\t"));
    }
}
