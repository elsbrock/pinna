use std::fmt;

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Box<str>);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into().into_boxed_str())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn last4(&self) -> String {
        let start = self.0.char_indices().rev().nth(3).map_or(0, |(i, _)| i);
        self.0[start..].to_owned()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(..{})", self.last4())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debug_shows_only_suffix() {
        assert_eq!(
            format!("{:?}", Secret::new("sk-abcd12345678")),
            "Secret(..5678)"
        );
    }
}
