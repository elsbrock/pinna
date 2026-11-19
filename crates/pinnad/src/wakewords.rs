//! The wake words pinnad can listen for (ADR 0013): the built-in table, the
//! `[wake]` word from pinnad.toml, the custom words an owner added, and
//! loading their classifiers.

mod check;
mod custom;

use std::path::{Path, PathBuf};

use pinna_esphome::WakeWordInfo;
use pinna_wakeword::Keyword;

use crate::config;

pub use check::{BUSY, Checker};
pub use custom::{Entry, MAX_MODEL_BYTES, sha256_hex};

/// How many words listen at once: Home Assistant shows two dropdowns.
pub const MAX_ACTIVE: usize = 2;

/// The phrase of a custom word: 2 to 40 characters.
const PHRASE_CHARS: std::ops::RangeInclusive<usize> = 2..=40;
/// The longest slug in a custom word's id.
const SLUG_CHARS: usize = 32;

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
    /// Added by an owner (`AddWakeWord`), so it can be removed.
    pub custom: bool,
    /// Why the satellite can't listen for it; such a word can't be chosen.
    pub error: Option<String>,
}

impl Word {
    fn built_in(id: &str, phrase: &str, file: PathBuf) -> Self {
        Self {
            id: id.into(),
            phrase: phrase.into(),
            file,
            custom: false,
            error: None,
        }
    }
}

/// Why a new custom word is refused.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// The phrase breaks a rule.
    Phrase(String),
    /// There is no room for another custom word.
    Full(String),
}

impl Refusal {
    /// The sentence for the dashboard.
    pub fn into_message(self) -> String {
        match self {
            Self::Phrase(m) | Self::Full(m) => m,
        }
    }
}

/// What [`Catalogue::load`] made of the words asked for.
pub struct Loaded {
    pub keywords: Vec<Keyword>,
    /// The words that failed, and a sentence for each.
    pub failed: Vec<(String, String)>,
}

/// The words on offer: built-ins whose classifier is installed, the `[wake]`
/// word if it is not one of them, and the custom words, failed ones too.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalogue {
    words: Vec<Word>,
    /// The custom words as `index.toml` has them.
    entries: Vec<Entry>,
    /// Where the custom words live (`/var/lib/pinna/wakewords`); empty for none.
    dir: PathBuf,
}

impl Catalogue {
    pub fn new(cfg: &config::Wake) -> Self {
        let mut words: Vec<Word> = BUILT_IN
            .iter()
            .map(|(id, phrase, file)| Word::built_in(id, phrase, cfg.models.join(file)))
            .filter(|w| w.file.exists())
            .collect();
        if !BUILT_IN.iter().any(|(id, ..)| *id == cfg.id) {
            let file = cfg.models.join(&cfg.keyword);
            if file.exists() {
                words.push(Word::built_in(&cfg.id, &cfg.phrase, file));
            }
        }
        Self {
            words,
            ..Self::default()
        }
    }

    /// [`new`](Self::new) plus the custom words in `dir`. A model that was
    /// loading when pinnad last stopped crashed it: that word is marked failed
    /// for good, so the next start can't crash on it again.
    pub fn open(cfg: &config::Wake, dir: &Path) -> Self {
        let mut catalogue = Self::new(cfg);
        catalogue.dir = dir.to_owned();
        custom::remove_staged(dir);
        let mut entries = custom::read_index(dir);
        if let Some(id) = custom::take_loading_mark(dir) {
            tracing::error!("custom wake word {id:?} crashed pinnad while loading; not loading it");
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.failed = Some(custom::CRASHED.into());
                if let Err(e) = custom::write_index(dir, &entries) {
                    tracing::error!("custom wake words: saving the failure: {e}");
                }
            }
        }
        for entry in entries {
            if catalogue.any(&entry.id).is_some() {
                tracing::warn!("custom wake word {:?} repeats an id; ignoring it", entry.id);
                continue;
            }
            let file = dir.join(&entry.file);
            let error = entry.failed.clone().or_else(|| {
                (!file.exists()).then(|| {
                    tracing::error!(
                        "custom wake word {}: {} is missing",
                        entry.phrase,
                        file.display()
                    );
                    custom::MISSING.to_owned()
                })
            });
            catalogue.words.push(Word {
                id: entry.id.clone(),
                phrase: entry.phrase.clone(),
                file,
                custom: true,
                error,
            });
            catalogue.entries.push(entry);
        }
        catalogue
    }

    #[cfg(test)]
    pub fn from_words(words: Vec<Word>) -> Self {
        Self {
            words,
            ..Self::default()
        }
    }

    /// Every word, for the dashboard.
    pub fn words(&self) -> &[Word] {
        &self.words
    }

    /// The words that can listen.
    pub fn listening(&self) -> impl Iterator<Item = &Word> {
        self.words.iter().filter(|w| w.error.is_none())
    }

    /// The words Home Assistant is offered: those that can listen.
    pub fn for_home_assistant(&self) -> Vec<WakeWordInfo> {
        self.listening()
            .map(|w| WakeWordInfo {
                id: w.id.clone(),
                wake_word: w.phrase.clone(),
                trained_languages: vec!["en".into()],
            })
            .collect()
    }

    fn any(&self, id: &str) -> Option<&Word> {
        self.words.iter().find(|w| w.id == id)
    }

    /// A word that can listen.
    pub fn get(&self, id: &str) -> Option<&Word> {
        self.any(id).filter(|w| w.error.is_none())
    }

    /// The word `id`, whether it can listen or not.
    pub fn find(&self, id: &str) -> Option<&Word> {
        self.any(id)
    }

    /// The phrase for `id`, or the id itself for a word no longer on offer.
    pub fn phrase<'a>(&'a self, id: &'a str) -> &'a str {
        self.any(id).map_or(id, |w| w.phrase.as_str())
    }

    /// The phrases of `ids` for display: "Alexa or Hey Jarvis".
    pub fn phrases(&self, ids: &[String]) -> String {
        ids.iter()
            .map(|id| self.phrase(id))
            .collect::<Vec<_>>()
            .join(" or ")
    }

    /// Check a choice from the control API: words that can listen, no
    /// repeats, at most [`MAX_ACTIVE`]. The error is a sentence for the
    /// dashboard.
    pub fn validate(&self, ids: &[String]) -> Result<(), String> {
        if ids.len() > MAX_ACTIVE {
            return Err(format!(
                "At most {MAX_ACTIVE} wake words can listen at once."
            ));
        }
        for (i, id) in ids.iter().enumerate() {
            match self.any(id) {
                None => return Err(format!("This satellite has no wake word \"{id}\".")),
                Some(Word {
                    phrase,
                    error: Some(why),
                    ..
                }) => return Err(format!("\"{phrase}\" can't listen. {why}")),
                Some(_) => (),
            }
            if ids[..i].contains(id) {
                return Err(format!("\"{}\" is chosen twice.", self.phrase(id)));
            }
        }
        Ok(())
    }

    /// Keep the words that can listen, without repeats, at most
    /// [`MAX_ACTIVE`], in order. What is dropped is logged (a word removed
    /// from the image, say).
    pub fn sanitize(&self, ids: &[String]) -> Vec<String> {
        let mut kept: Vec<String> = Vec::new();
        for id in ids {
            if self.get(id).is_none() {
                tracing::warn!("wake word {id:?} is not available; not listening for it");
            } else if !kept.contains(id) && kept.len() < MAX_ACTIVE {
                kept.push(id.clone());
            }
        }
        kept
    }

    /// Load the classifiers for `ids`. Blocking: each takes up to a few hundred
    /// milliseconds on the Echo Dot. A word that fails is logged and left out,
    /// and reported in [`Loaded::failed`]. A custom word's model is marked as
    /// loading while it loads (see [`open`](Self::open)).
    pub fn load(&self, ids: &[String]) -> Loaded {
        let mut loaded = Loaded {
            keywords: Vec::new(),
            failed: Vec::new(),
        };
        for id in ids {
            let Some(w) = self.get(id) else { continue };
            match self.load_word(w) {
                Ok(k) => loaded.keywords.push(k),
                Err(why) => {
                    tracing::error!("wake word {}: {why}", w.phrase);
                    loaded.failed.push((id.clone(), why));
                }
            }
        }
        loaded
    }

    fn load_word(&self, w: &Word) -> Result<Keyword, String> {
        let bytes = std::fs::read(&w.file)
            .map_err(|e| format!("Its model can't be read: {}: {e}.", w.file.display()))?;
        if !w.custom {
            return Keyword::from_bytes(w.id.clone(), &bytes).map_err(|e| format!("{e:#}"));
        }
        if self
            .entries
            .iter()
            .find(|e| e.id == w.id)
            .is_some_and(|e| e.sha256 != sha256_hex(&bytes))
        {
            return Err(custom::CHANGED.into());
        }
        custom::mark_loading(&self.dir, &w.id);
        let keyword = Keyword::from_bytes(w.id.clone(), &bytes);
        custom::clear_loading(&self.dir);
        keyword.map_err(|e| format!("Its model didn't load: {e:#}."))
    }

    /// This catalogue with the `failed` words ([`Loaded::failed`]) unable to
    /// listen.
    pub fn with_failed(&self, failed: &[(String, String)]) -> Self {
        let mut next = self.clone();
        for (id, why) in failed {
            if let Some(w) = next.words.iter_mut().find(|w| w.id == *id) {
                w.error = Some(why.clone());
            }
        }
        next
    }

    /// Where the custom words are kept.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The rules for a new custom word (ADR 0013): the phrase with single
    /// spaces, 2 to 40 characters without control characters, unique among
    /// all words ignoring case, spaces and punctuation, and room for it.
    /// Returns the phrase and the new id. The error is a sentence for the
    /// dashboard.
    pub fn plan_custom(&self, phrase: &str) -> Result<(String, String), Refusal> {
        let phrase_error = |m: &str| Refusal::Phrase(m.to_owned());
        let phrase = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
        let phrase = phrase.as_str();
        if !PHRASE_CHARS.contains(&phrase.chars().count()) {
            return Err(phrase_error(
                "A wake word phrase must be 2 to 40 characters long.",
            ));
        }
        if phrase.chars().any(char::is_control) {
            return Err(phrase_error(
                "A wake word phrase can't contain control characters.",
            ));
        }
        // Home Assistant's dropdowns would show "Hey Custom" and "hey custom."
        // as the same word.
        let key = phrase_key(phrase);
        if let Some(w) = self.words.iter().find(|w| phrase_key(&w.phrase) == key) {
            let fix = if w.custom {
                "Choose another phrase, or remove that word first."
            } else {
                "Choose another phrase."
            };
            return Err(Refusal::Phrase(format!(
                "This satellite already has the wake word \"{}\". {fix}",
                w.phrase
            )));
        }
        if self.entries.len() >= custom::MAX_CUSTOM {
            return Err(Refusal::Full(format!(
                "A satellite can have at most {} custom wake words. Remove one first.",
                custom::MAX_CUSTOM
            )));
        }
        let base = match slug(phrase).as_str() {
            "" => "custom".to_owned(),
            slug => format!("custom_{slug}"),
        };
        let mut id = base.clone();
        let mut n = 2;
        while self.any(&id).is_some() {
            id = format!("{base}_{n}");
            n += 1;
        }
        Ok((phrase.to_owned(), id))
    }

    /// This catalogue with a custom word added. The model file and the index
    /// are the caller's to write ([`save_index`](Self::save_index)).
    pub fn with_added(&self, entry: Entry) -> Self {
        let mut next = self.clone();
        next.words.push(Word {
            id: entry.id.clone(),
            phrase: entry.phrase.clone(),
            file: self.dir.join(&entry.file),
            custom: true,
            error: None,
        });
        next.entries.push(entry);
        next
    }

    /// This catalogue without the custom word `id`.
    pub fn without(&self, id: &str) -> Self {
        let mut next = self.clone();
        next.words.retain(|w| w.id != id);
        next.entries.retain(|e| e.id != id);
        next
    }

    /// Save the custom words to `index.toml`.
    pub fn save_index(&self) -> std::io::Result<()> {
        custom::write_index(&self.dir, &self.entries)
    }
}

/// What two phrases must not share: their letters and digits, lowercase.
fn phrase_key(phrase: &str) -> String {
    phrase
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Lowercase ASCII letters and digits of `phrase`, other runs as one `_`,
/// without `_` at the ends, at most [`SLUG_CHARS`] long.
fn slug(phrase: &str) -> String {
    let mut slug = String::new();
    for c in phrase.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    slug.truncate(SLUG_CHARS);
    slug.trim_matches('_').to_owned()
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
    let word = |id: &str, phrase: &str| Word::built_in(id, phrase, PathBuf::from("/nonexistent"));
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

    fn entry(id: &str, phrase: &str, file: &str) -> Entry {
        Entry {
            id: id.into(),
            phrase: phrase.into(),
            file: file.into(),
            sha256: sha256_hex(b"model"),
            failed: None,
        }
    }

    /// A models directory with Alexa, and an empty custom directory.
    fn with_custom() -> (config::Wake, PathBuf, PathBuf) {
        let models = models(&["alexa_v0.1.onnx"]);
        let custom = models.join("wakewords");
        std::fs::create_dir_all(&custom).unwrap();
        (wake(models.clone()), models, custom)
    }

    #[test]
    fn slugs_and_ids() {
        assert_eq!(slug("Hey Custom!"), "hey_custom");
        assert_eq!(slug("  --Ünï  cödé--"), "n_c_d");
        assert_eq!(slug("ÄÖ"), "");
        assert_eq!(slug(&"a".repeat(50)).len(), 32);
        assert_eq!(slug(&format!("{} b", "a".repeat(31))), "a".repeat(31));

        let c = Catalogue::from_words(vec![]);
        assert_eq!(
            c.plan_custom("  Hey   Custom ").unwrap(),
            ("Hey Custom".into(), "custom_hey_custom".into())
        );
        assert_eq!(c.plan_custom("日本").unwrap().1, "custom");
        // Different phrases can share a slug; the id takes a number then.
        let c = c
            .with_added(entry("custom_hey_custom", "Hey Custom", "a.onnx"))
            .with_added(entry("custom_hey_custom_2", "Hey Ü Custom", "b.onnx"));
        assert_eq!(
            c.plan_custom("Hey Ö Custom").unwrap().1,
            "custom_hey_custom_3"
        );
    }

    #[test]
    fn the_rules_for_a_new_phrase() {
        let (cfg, dir, custom) = with_custom();
        let mut c = Catalogue::open(&cfg, &custom);
        assert!(
            c.plan_custom("x")
                .unwrap_err()
                .into_message()
                .contains("2 to 40")
        );
        assert!(
            c.plan_custom(&"x".repeat(41))
                .unwrap_err()
                .into_message()
                .contains("2 to 40")
        );
        assert!(c.plan_custom(&"é".repeat(40)).is_ok());
        assert!(
            c.plan_custom("Hey\u{7}there")
                .unwrap_err()
                .into_message()
                .contains("control")
        );
        assert!(matches!(
            c.plan_custom(" ALEXA "),
            Err(Refusal::Phrase(m)) if m == "This satellite already has the wake word \"Alexa\". Choose another phrase."
        ));
        let mine = c.with_added(entry("custom_hey_custom", "Hey Custom", "a.onnx"));
        for twin in ["hey custom", "HEY  CUSTOM", "Hey, Custom.", "Hey-Custom"] {
            assert!(
                matches!(
                    mine.plan_custom(twin),
                    Err(Refusal::Phrase(ref m)) if m.contains("\"Hey Custom\"") && m.contains("remove that word")
                ),
                "{twin}"
            );
        }
        for i in 0..custom::MAX_CUSTOM {
            let (phrase, id) = c.plan_custom(&format!("Word {i}")).unwrap();
            let mut e = entry(&id, &phrase, "x.onnx");
            // Failed words count against the limit and their phrases are taken.
            e.failed = (i == 0).then(|| "No.".into());
            c = c.with_added(e);
        }
        assert!(matches!(c.plan_custom("Word 0"), Err(Refusal::Phrase(_))));
        assert!(matches!(
            c.plan_custom("Another"),
            Err(Refusal::Full(m)) if m.contains("at most 8")
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn custom_words_survive_a_restart() {
        let (cfg, dir, custom) = with_custom();
        // Nothing yet: no index.
        assert_eq!(ids(&Catalogue::open(&cfg, &custom)), ["alexa_v0.1"]);

        std::fs::write(custom.join("custom_hey.onnx"), b"model").unwrap();
        let c = Catalogue::open(&cfg, &custom).with_added(entry(
            "custom_hey",
            "Hey",
            "custom_hey.onnx",
        ));
        c.save_index().unwrap();
        let c = Catalogue::open(&cfg, &custom);
        assert_eq!(ids(&c), ["alexa_v0.1", "custom_hey"]);
        let hey = c.get("custom_hey").unwrap();
        assert!(hey.custom && hey.error.is_none());
        assert_eq!(c.for_home_assistant().len(), 2);

        // A word whose file is gone fails; it is on the list but not offered.
        std::fs::remove_file(custom.join("custom_hey.onnx")).unwrap();
        let c = Catalogue::open(&cfg, &custom);
        assert_eq!(c.words()[1].error.as_deref(), Some(custom::MISSING));
        assert!(c.get("custom_hey").is_none());
        assert_eq!(c.for_home_assistant().len(), 1);
        assert!(c.sanitize(&["custom_hey".into()]).is_empty());
        let why = c.validate(&["custom_hey".into()]).unwrap_err();
        assert!(
            why.contains("\"Hey\"") && why.contains("model file is missing"),
            "{why}"
        );
        // Removing it leaves the rest.
        assert_eq!(ids(&c.without("custom_hey")), ["alexa_v0.1"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_upload_left_from_a_test_is_deleted_at_start() {
        let (cfg, dir, custom) = with_custom();
        std::fs::write(custom.join(".upload-0123456789abcdef"), b"model").unwrap();
        std::fs::write(custom.join("custom_hey.onnx"), b"model").unwrap();
        Catalogue::open(&cfg, &custom);
        assert!(!custom.join(".upload-0123456789abcdef").exists());
        assert!(custom.join("custom_hey.onnx").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_corrupt_index_means_no_custom_words() {
        let (cfg, dir, custom) = with_custom();
        std::fs::write(custom.join("index.toml"), "words = [ {{{").unwrap();
        assert_eq!(ids(&Catalogue::open(&cfg, &custom)), ["alexa_v0.1"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_loading_mark_left_behind_fails_that_word_for_good() {
        let (cfg, dir, custom) = with_custom();
        std::fs::write(custom.join("custom_hey.onnx"), b"model").unwrap();
        Catalogue::open(&cfg, &custom)
            .with_added(entry("custom_hey", "Hey", "custom_hey.onnx"))
            .save_index()
            .unwrap();
        custom::mark_loading(&custom, "custom_hey");

        let c = Catalogue::open(&cfg, &custom);
        assert_eq!(c.words()[1].error.as_deref(), Some(custom::CRASHED));
        assert!(!custom.join("loading").exists());
        // The failure is in the index, so a later start still has it.
        let c = Catalogue::open(&cfg, &custom);
        assert_eq!(c.words()[1].error.as_deref(), Some(custom::CRASHED));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_changed_or_broken_model_fails_when_it_loads() {
        let (cfg, dir, custom) = with_custom();
        std::fs::write(custom.join("custom_hey.onnx"), b"not the model").unwrap();
        let c = Catalogue::open(&cfg, &custom).with_added(entry(
            "custom_hey",
            "Hey",
            "custom_hey.onnx",
        ));
        let loaded = c.load(&["custom_hey".into()]);
        assert!(loaded.keywords.is_empty());
        assert_eq!(
            loaded.failed,
            [("custom_hey".into(), custom::CHANGED.into())]
        );
        assert!(!custom.join("loading").exists());

        // The right bytes that aren't a model: the load error is the sentence.
        std::fs::write(custom.join("custom_hey.onnx"), b"model").unwrap();
        let loaded = c.load(&["custom_hey".into()]);
        assert!(loaded.failed[0].1.starts_with("Its model didn't load: "));
        assert!(!custom.join("loading").exists());
        let c = c.with_failed(&loaded.failed);
        assert!(c.get("custom_hey").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
