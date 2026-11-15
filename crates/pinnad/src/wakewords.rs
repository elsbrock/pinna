//! The wake words pinnad can listen for (ADR 0013): the built-in table, the
//! `[wake]` word from pinnad.toml, and loading their classifiers.

use std::path::PathBuf;

use pinna_wakeword::Keyword;

use crate::config;

/// How many words listen at once: Home Assistant shows two dropdowns.
pub const MAX_ACTIVE: usize = 2;

/// Built-in words: the id Home Assistant knows, the phrase, the classifier in
/// the models directory (`scripts/fetch-models.sh`).
const BUILT_IN: &[(&str, &str, &str)] = &[
    ("alexa_v0.1", "Alexa", "alexa_v0.1.onnx"),
    ("hey_jarvis_v0.1", "Hey Jarvis", "hey_jarvis_v0.1.onnx"),
    ("hey_mycroft_v0.1", "Hey Mycroft", "hey_mycroft_v0.1.onnx"),
    ("hey_rhasspy_v0.1", "Hey Rhasspy", "hey_rhasspy_v0.1.onnx"),
    ("okay_nabu", "Okay Nabu", "okay_nabu.tflite"),
];

#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub id: String,
    pub phrase: String,
    pub file: PathBuf,
}

/// The words on offer: built-ins whose classifier is installed, plus the
/// `[wake]` word if it is not one of them.
#[derive(Debug, Clone, Default)]
pub struct Catalogue {
    words: Vec<Word>,
}

impl Catalogue {
    pub fn new(cfg: &config::Wake) -> Self {
        let mut words: Vec<Word> = BUILT_IN
            .iter()
            .map(|(id, phrase, file)| Word {
                id: (*id).into(),
                phrase: (*phrase).into(),
                file: cfg.models.join(file),
            })
            .filter(|w| w.file.exists())
            .collect();
        if !BUILT_IN.iter().any(|(id, ..)| *id == cfg.id) {
            let file = cfg.models.join(&cfg.keyword);
            if file.exists() {
                words.push(Word {
                    id: cfg.id.clone(),
                    phrase: cfg.phrase.clone(),
                    file,
                });
            }
        }
        Self { words }
    }

    #[cfg(test)]
    pub fn from_words(words: Vec<Word>) -> Self {
        Self { words }
    }

    pub fn words(&self) -> &[Word] {
        &self.words
    }

    pub fn get(&self, id: &str) -> Option<&Word> {
        self.words.iter().find(|w| w.id == id)
    }

    /// The phrase for `id`, or the id itself for a word no longer on offer.
    pub fn phrase<'a>(&'a self, id: &'a str) -> &'a str {
        self.get(id).map_or(id, |w| w.phrase.as_str())
    }

    /// The phrases of `ids` for display: "Alexa or Hey Jarvis".
    pub fn phrases(&self, ids: &[String]) -> String {
        ids.iter()
            .map(|id| self.phrase(id))
            .collect::<Vec<_>>()
            .join(" or ")
    }

    /// Check a choice from the control API: known words, no repeats, at most
    /// [`MAX_ACTIVE`]. The error is a sentence for the dashboard.
    pub fn validate(&self, ids: &[String]) -> Result<(), String> {
        if ids.len() > MAX_ACTIVE {
            return Err(format!(
                "At most {MAX_ACTIVE} wake words can listen at once."
            ));
        }
        for (i, id) in ids.iter().enumerate() {
            if self.get(id).is_none() {
                return Err(format!("This satellite has no wake word \"{id}\"."));
            }
            if ids[..i].contains(id) {
                return Err(format!("\"{}\" is chosen twice.", self.phrase(id)));
            }
        }
        Ok(())
    }

    /// Keep the words on offer, without repeats, at most [`MAX_ACTIVE`], in
    /// order. What is dropped is logged (a word removed from the image, say).
    pub fn sanitize(&self, ids: &[String]) -> Vec<String> {
        let mut kept: Vec<String> = Vec::new();
        for id in ids {
            if self.get(id).is_none() {
                tracing::warn!("wake word {id:?} is not installed; not listening for it");
            } else if !kept.contains(id) && kept.len() < MAX_ACTIVE {
                kept.push(id.clone());
            }
        }
        kept
    }

    /// Load the classifiers for `ids`. Blocking: each takes up to a few hundred
    /// milliseconds on the Echo Dot. A word that fails is logged and left out.
    pub fn load(&self, ids: &[String]) -> Vec<Keyword> {
        ids.iter()
            .filter_map(|id| {
                let w = self.get(id)?;
                let loaded = std::fs::read(&w.file)
                    .map_err(anyhow::Error::from)
                    .and_then(|bytes| Keyword::from_bytes(id.clone(), &bytes));
                match loaded {
                    Ok(k) => Some(k),
                    Err(e) => {
                        tracing::error!("wake word {}: {e:#}", w.phrase);
                        None
                    }
                }
            })
            .collect()
    }
}

/// Keywords for the capture loop. A load can finish after a newer one, so
/// only a later generation replaces the detector's keywords.
pub struct KeywordSet {
    pub generation: u64,
    pub keywords: Vec<Keyword>,
}

/// Two words for tests elsewhere in pinnad; their files don't exist.
#[cfg(test)]
pub fn test_catalogue() -> std::sync::Arc<Catalogue> {
    let word = |id: &str, phrase: &str| Word {
        id: id.into(),
        phrase: phrase.into(),
        file: PathBuf::from("/nonexistent"),
    };
    std::sync::Arc::new(Catalogue::from_words(vec![
        word("alexa_v0.1", "Alexa"),
        word("hey_jarvis_v0.1", "Hey Jarvis"),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models(files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pinnad-wakewords-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        dir
    }

    fn wake(models: PathBuf) -> config::Wake {
        config::Wake {
            models,
            ..Default::default()
        }
    }

    fn ids(c: &Catalogue) -> Vec<&str> {
        c.words().iter().map(|w| w.id.as_str()).collect()
    }

    #[test]
    fn offers_the_installed_built_ins() {
        let dir = models(&["alexa_v0.1.onnx", "okay_nabu.tflite"]);
        let c = Catalogue::new(&wake(dir.clone()));
        assert_eq!(ids(&c), ["alexa_v0.1", "okay_nabu"]);
        assert_eq!(c.phrase("okay_nabu"), "Okay Nabu");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn adds_a_configured_word_that_is_not_built_in() {
        let dir = models(&["alexa_v0.1.onnx", "computer.onnx"]);
        let cfg = config::Wake {
            keyword: "computer.onnx".into(),
            id: "computer".into(),
            phrase: "Computer".into(),
            ..wake(dir.clone())
        };
        let c = Catalogue::new(&cfg);
        assert_eq!(ids(&c), ["alexa_v0.1", "computer"]);
        assert_eq!(c.phrase("computer"), "Computer");
        // A configured built-in keeps its own phrase and isn't offered twice.
        let cfg = config::Wake {
            phrase: "Hey Alexa".into(),
            ..wake(dir.clone())
        };
        let c = Catalogue::new(&cfg);
        assert_eq!(ids(&c), ["alexa_v0.1"]);
        assert_eq!(c.phrase("alexa_v0.1"), "Alexa");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn no_models_no_words() {
        let dir = models(&[]);
        assert!(Catalogue::new(&wake(dir.clone())).words().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sanitize_and_validate() {
        let dir = models(&[
            "alexa_v0.1.onnx",
            "hey_jarvis_v0.1.onnx",
            "hey_mycroft_v0.1.onnx",
        ]);
        let c = Catalogue::new(&wake(dir.clone()));
        let v = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert_eq!(
            c.sanitize(&v(&[
                "gone",
                "hey_jarvis_v0.1",
                "hey_jarvis_v0.1",
                "alexa_v0.1",
                "hey_mycroft_v0.1"
            ])),
            v(&["hey_jarvis_v0.1", "alexa_v0.1"])
        );
        assert!(c.sanitize(&[]).is_empty());

        assert_eq!(c.validate(&v(&["alexa_v0.1", "hey_jarvis_v0.1"])), Ok(()));
        assert_eq!(c.validate(&[]), Ok(()));
        assert!(
            c.validate(&v(&["gone"]))
                .unwrap_err()
                .contains("no wake word \"gone\"")
        );
        assert!(
            c.validate(&v(&["alexa_v0.1", "alexa_v0.1"]))
                .unwrap_err()
                .contains("\"Alexa\" is chosen twice")
        );
        assert!(
            c.validate(&v(&["alexa_v0.1", "hey_jarvis_v0.1", "hey_mycroft_v0.1"]))
                .unwrap_err()
                .contains("At most 2")
        );
        assert_eq!(
            c.phrases(&v(&["alexa_v0.1", "hey_jarvis_v0.1"])),
            "Alexa or Hey Jarvis"
        );
        assert_eq!(c.phrases(&[]), "");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
