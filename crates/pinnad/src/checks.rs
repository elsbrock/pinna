//! Shared validation for device commands (ADR 0017).

pub fn percent(name: &str, v: u32) -> Result<u8, String> {
    if v <= 100 {
        Ok(v as u8)
    } else {
        Err(format!("{name} must be between 0 and 100"))
    }
}

pub fn media_url(s: &str) -> Result<String, String> {
    let url = s.trim();
    if url.len() <= 2048
        && url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
    {
        Ok(url.to_owned())
    } else {
        Err("the URL must start with http:// or https:// and be at most 2048 characters".into())
    }
}

/// The longest text `speak` says (architecture §7, size caps).
pub const SPEAK_MAX: usize = 500;

/// A text for the assistant's voice to say, trimmed.
pub fn speak_text(s: &str) -> Result<String, String> {
    let text = s.trim();
    if text.is_empty() {
        return Err("Enter the text to say.".into());
    }
    if text.chars().count() > SPEAK_MAX {
        return Err(format!(
            "The text to say is at most {SPEAK_MAX} characters."
        ));
    }
    Ok(text.to_owned())
}

/// The longest question typed on the dashboard.
pub const QUESTION_MAX: usize = 1_000;

/// A typed question, trimmed.
pub fn question(s: &str) -> Result<String, String> {
    let question = s.trim();
    if question.is_empty() {
        return Err("Type a question.".into());
    }
    if question.chars().count() > QUESTION_MAX {
        return Err(format!("A question is at most {QUESTION_MAX} characters."));
    }
    Ok(question.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentage_bounds() {
        assert_eq!(percent("volume", 0), Ok(0));
        assert_eq!(percent("volume", 100), Ok(100));
        assert_eq!(
            percent("volume", 101).unwrap_err(),
            "volume must be between 0 and 100"
        );
    }

    #[test]
    fn playback_urls() {
        assert_eq!(
            media_url(" https://radio.test/ ").unwrap(),
            "https://radio.test/"
        );
        let full = format!("http://x/{}", "a".repeat(2039));
        assert!(media_url(&full).is_ok());
        assert!(media_url(&(full + "a")).is_err());
        assert!(media_url("file:///etc/hosts").is_err());
    }

    #[test]
    fn typed_questions() {
        assert_eq!(question(" What time is it? ").unwrap(), "What time is it?");
        assert_eq!(question("\t").unwrap_err(), "Type a question.");
        assert!(question(&"ß".repeat(QUESTION_MAX)).is_ok());
        assert_eq!(
            question(&"ß".repeat(QUESTION_MAX + 1)).unwrap_err(),
            "A question is at most 1000 characters."
        );
    }

    #[test]
    fn spoken_texts() {
        assert_eq!(speak_text("  Hello.  ").unwrap(), "Hello.");
        assert_eq!(speak_text(" \n ").unwrap_err(), "Enter the text to say.");
        assert!(speak_text(&"ä".repeat(SPEAK_MAX)).is_ok());
        assert_eq!(
            speak_text(&"ä".repeat(SPEAK_MAX + 1)).unwrap_err(),
            "The text to say is at most 500 characters."
        );
    }
}
