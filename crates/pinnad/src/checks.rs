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
}
