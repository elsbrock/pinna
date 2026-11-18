//! The acknowledgment a slow agent plays (ADR 0017 §"The pipeline").
//!
//! The phrase is the owner's. While the owner keeps the default, it is the
//! language's own from a small table that falls back to English. It is
//! synthesised once for each voice, language and phrase and kept in
//! `cache/` beside the settings, so it plays without waiting for a service.
//! The current file and the one before it stay on disk, because a run may
//! still hold the older path; older files are deleted.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::StreamExt;
use pinna_assist::config::{Ack, AssistConfig};
use pinna_assist::pipeline::Pipeline;
use pinna_assist::presets::PresetId;
use pinna_assist::reply::AudioBody;
use pinna_assist::tts::Tts;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::settings;

/// An acknowledgment file is at most this large.
pub const MAX_BYTES: usize = 512 * 1024;
/// From the synthesis request to the last byte.
const SYNTHESIS_TIMEOUT: Duration = Duration::from_secs(30);
/// After a failure, try again after each of these pauses while the same
/// acknowledgment is still wanted: at boot the network may not be up yet.
const RETRIES: [Duration; 3] = [
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];

/// One phrase per language, by primary language subtag. The first row is the
/// fallback and matches the stored default.
const PHRASES: &[(&str, &str)] = &[
    ("en", "One moment."),
    ("cs", "Okamžik."),
    ("da", "Et øjeblik."),
    ("de", "Einen Moment."),
    ("el", "Μια στιγμή."),
    ("es", "Un momento."),
    ("fi", "Hetki."),
    ("fr", "Un instant."),
    ("hu", "Egy pillanat."),
    ("it", "Un momento."),
    ("ja", "少々お待ちください。"),
    ("ko", "잠시만요."),
    ("nb", "Et øyeblikk."),
    ("nl", "Een moment."),
    ("nn", "Eit augeblikk."),
    ("no", "Et øyeblikk."),
    ("pl", "Chwileczkę."),
    ("pt", "Um momento."),
    ("ro", "Un moment."),
    ("ru", "Одну минуту."),
    ("sk", "Okamih."),
    ("sv", "Ett ögonblick."),
    ("tr", "Bir saniye."),
    ("uk", "Хвилинку."),
    ("zh", "请稍等。"),
];

/// The language's phrase, or English's.
fn table(language: Option<&str>) -> &'static str {
    let primary = language.map(pinna_assist::config::primary_language);
    PHRASES
        .iter()
        .find(|(tag, _)| primary.as_deref() == Some(*tag))
        .unwrap_or(&PHRASES[0])
        .1
}

/// The phrase to say: the owner's own, or the language's while the owner
/// keeps the default.
pub fn phrase(ack: &Ack, language: Option<&str>) -> String {
    let own = ack.phrase.trim();
    if own.is_empty() || own == Ack::default().phrase {
        table(language).to_owned()
    } else {
        own.to_owned()
    }
}

/// What an acknowledgment file is made from. Any change makes another file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    preset: PresetId,
    /// Another server may have a voice of the same name.
    base_url: String,
    voice: String,
    /// The slot's own speech model; empty is the preset's.
    model: String,
    language: Option<String>,
    phrase: String,
}

impl Source {
    /// The acknowledgment the configuration asks for: it is on, the brain is
    /// one that acknowledges, and the text-to-speech slot passed its check.
    pub fn wanted(
        config: &AssistConfig,
        pipeline: Option<&Pipeline>,
        tts_ready: bool,
    ) -> Option<Self> {
        let acknowledges = pipeline?.behaviour.limits.ack_after.is_some();
        if !config.ack.enabled || !acknowledges || !tts_ready {
            return None;
        }
        let slot = config.tts.as_ref()?;
        let connection = config
            .connections
            .iter()
            .find(|connection| connection.id == slot.connection)?;
        let language = config.language.clone();
        Some(Self {
            preset: connection.preset,
            base_url: connection.base_url.clone(),
            voice: slot.model.clone(),
            model: slot.speech_model.clone().unwrap_or_default(),
            phrase: phrase(&config.ack, language.as_deref()),
            language,
        })
    }

    pub fn phrase(&self) -> &str {
        &self.phrase
    }

    /// `ack-<16 hex digits of the inputs' SHA-256>.mp3`.
    pub fn file_name(&self) -> String {
        let mut hash = Sha256::new();
        let parts = [
            self.preset.label(),
            &self.base_url,
            &self.voice,
            self.language.as_deref().unwrap_or(""),
            &self.phrase,
        ];
        // The model only when the slot names one, so the files made before
        // it could be named keep their names.
        let model = (!self.model.is_empty()).then_some(self.model.as_str());
        for part in parts.into_iter().chain(model) {
            // Length first, so the parts can't run into each other.
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part.as_bytes());
        }
        let digest = hash.finalize();
        let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        format!("ack-{hex}.mp3")
    }
}

/// The acknowledgment files in one directory.
pub struct AckCache {
    dir: PathBuf,
    retries: Vec<Duration>,
    /// Counts refreshes; a newer one takes over retrying from an older one.
    refreshes: AtomicU64,
    /// Held across a synthesis, so files are made one at a time.
    files: Mutex<Files>,
    /// Where a failure is reported: a warning, but a counter in tests.
    report: fn(&str),
}

#[derive(Default)]
struct Files {
    /// The file the configuration played last. A new file replaces it, and
    /// it is kept beside the new one.
    current: Option<PathBuf>,
    /// The file whose failure was last reported, so it is reported once.
    reported: Option<PathBuf>,
}

impl AckCache {
    pub fn new(dir: PathBuf) -> Self {
        Self::with(dir, RETRIES.to_vec(), warn)
    }

    fn with(dir: PathBuf, retries: Vec<Duration>, report: fn(&str)) -> Self {
        Self {
            dir,
            retries,
            refreshes: AtomicU64::new(0),
            files: Mutex::new(Files::default()),
            report,
        }
    }

    /// The file for `source`, if it is on disk.
    pub fn path(&self, source: &Source) -> Option<PathBuf> {
        let path = self.dir.join(source.file_name());
        path.is_file().then_some(path)
    }

    /// Make the file `wanted` asks for unless it is on disk, and say whether
    /// a new file was written. `wanted` is asked again before every attempt,
    /// so a change meanwhile is followed. A failure is reported once per
    /// file and tried again on the retry schedule, until a newer refresh
    /// takes over.
    pub async fn refresh(&self, wanted: impl Fn() -> Option<(Source, Arc<dyn Tts>)>) -> bool {
        let refresh = self.refreshes.fetch_add(1, Ordering::Relaxed) + 1;
        let mut pauses = self.retries.iter();
        loop {
            if let Some(written) = self.attempt(&wanted).await {
                return written;
            }
            let Some(pause) = pauses.next() else {
                return false;
            };
            tokio::time::sleep(*pause).await;
            if self.refreshes.load(Ordering::Relaxed) != refresh {
                return false;
            }
        }
    }

    /// Whether a file was written, or `None` when making it failed.
    async fn attempt(&self, wanted: &impl Fn() -> Option<(Source, Arc<dyn Tts>)>) -> Option<bool> {
        let mut files = self.files.lock().await;
        let Some((source, tts)) = wanted() else {
            return Some(false);
        };
        let path = self.dir.join(source.file_name());
        if path.is_file() {
            files.current = Some(path);
            return Some(false);
        }
        let made = match synthesise(tts.as_ref(), source.phrase()).await {
            Ok(audio) => {
                let keep = files.current.clone();
                let (dir, target) = (self.dir.clone(), path.clone());
                tokio::task::spawn_blocking(move || write(&dir, &target, &audio, keep.as_deref()))
                    .await
                    .unwrap_or_else(|e| Err(io::Error::other(e)))
                    .map_err(|e| format!("it couldn't be saved: {e}"))
            }
            Err(why) => Err(why),
        };
        match made {
            Ok(()) => {
                files.current = Some(path);
                files.reported = None;
                Some(true)
            }
            Err(why) => {
                if files.reported.as_ref() != Some(&path) {
                    (self.report)(&format!(
                        "the acknowledgment \"{}\" wasn't made, so none plays: {why}",
                        source.phrase()
                    ));
                    files.reported = Some(path);
                }
                None
            }
        }
    }
}

fn warn(why: &str) {
    tracing::warn!("{why}");
}

/// The phrase's audio, at most [`MAX_BYTES`].
async fn synthesise(tts: &dyn Tts, phrase: &str) -> Result<Vec<u8>, String> {
    let read = async {
        let audio = tts.speak(phrase).await.map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        match audio.body {
            AudioBody::Stream(mut body) => {
                while let Some(chunk) = body.next().await {
                    let chunk = chunk.map_err(|_| "the audio broke off".to_owned())?;
                    if chunk.len() > MAX_BYTES - bytes.len() {
                        return Err(too_large());
                    }
                    bytes.extend_from_slice(&chunk);
                }
            }
            AudioBody::File(path) => {
                bytes = tokio::fs::read(&path)
                    .await
                    .map_err(|e| format!("the audio can't be read: {e}"))?;
                if bytes.len() > MAX_BYTES {
                    return Err(too_large());
                }
            }
        }
        if bytes.is_empty() {
            return Err("the voice sent no audio".into());
        }
        Ok(bytes)
    };
    tokio::time::timeout(SYNTHESIS_TIMEOUT, read)
        .await
        .unwrap_or_else(|_| {
            Err(format!(
                "it took more than {} s",
                SYNTHESIS_TIMEOUT.as_secs()
            ))
        })
}

fn too_large() -> String {
    format!("the audio is larger than {} KiB", MAX_BYTES / 1024)
}

/// Write `audio` to `path`, then delete every other acknowledgment file in
/// `dir` but `keep`.
fn write(dir: &Path, path: &Path, audio: &[u8], keep: Option<&Path>) -> io::Result<()> {
    crate::store::write_durable(path, audio, settings::FILE_MODE)?;
    for entry in std::fs::read_dir(dir)? {
        let old = entry?.path();
        let name = old.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let ours = name.starts_with("ack-") && name.ends_with(".mp3");
        if ours
            && old != path
            && Some(old.as_path()) != keep
            && let Err(e) = std::fs::remove_file(&old)
        {
            tracing::debug!("an old acknowledgment stays: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::AtomicUsize;

    use bytes::Bytes;
    use futures::future::BoxFuture;
    use pinna_assist::config::{Connection, SlotConfig};
    use pinna_assist::error::{ProviderError, Reason, Slot};
    use pinna_assist::reply::{AudioKind, SpeechAudio};
    use pinna_assist::secrets::Secrets;

    use super::*;

    /// Says `audio:<phrase>`, after failing as often as asked.
    #[derive(Default)]
    struct FakeTts {
        said: StdMutex<Vec<String>>,
        failures: AtomicUsize,
        size: Option<usize>,
    }

    impl FakeTts {
        fn failing(times: usize) -> Self {
            Self {
                failures: AtomicUsize::new(times),
                ..Self::default()
            }
        }

        fn said(&self) -> Vec<String> {
            self.said.lock().unwrap().clone()
        }
    }

    impl Tts for FakeTts {
        fn speak<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<SpeechAudio, ProviderError>> {
            self.said.lock().unwrap().push(text.to_owned());
            let failed = self
                .failures
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
                .is_ok();
            let result = if failed {
                Err(ProviderError::new(
                    Slot::Tts,
                    PresetId::OpenAi,
                    Reason::Http(503),
                ))
            } else {
                let audio = match self.size {
                    Some(size) => Bytes::from(vec![0; size]),
                    None => Bytes::from(format!("audio:{text}")),
                };
                Ok(SpeechAudio {
                    kind: AudioKind::Ack,
                    mime: Some("audio/mpeg".into()),
                    body: AudioBody::Stream(Box::pin(futures::stream::iter([Ok(audio)]))),
                })
            };
            Box::pin(std::future::ready(result))
        }
    }

    fn source(phrase: &str) -> Source {
        Source {
            preset: PresetId::OpenAi,
            base_url: "https://api.openai.com/v1".into(),
            voice: "nova".into(),
            model: String::new(),
            language: Some("de".into()),
            phrase: phrase.into(),
        }
    }

    fn files(dir: &Path) -> BTreeSet<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .map(|e| e.unwrap().file_name().into_string().unwrap())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A cache whose failures are only counted, retrying after `retries`.
    fn cache(dir: PathBuf, retries: Vec<Duration>) -> AckCache {
        AckCache::with(dir, retries, |_| {})
    }

    /// The failures reported by the one test that counts them.
    static REPORTED: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn the_phrase_follows_the_language_and_falls_back_to_english() {
        let stock = Ack::default();
        assert_eq!(table(None), stock.phrase);
        assert_eq!(phrase(&stock, None), "One moment.");
        assert_eq!(phrase(&stock, Some("de")), "Einen Moment.");
        assert_eq!(phrase(&stock, Some("DE-at")), "Einen Moment.");
        assert_eq!(phrase(&stock, Some("pt_BR")), "Um momento.");
        assert_eq!(phrase(&stock, Some("xx")), "One moment.");
        assert_eq!(phrase(&stock, Some("en-GB")), "One moment.");

        // The owner's own phrase is said as it is, in any language.
        let own = Ack {
            enabled: true,
            phrase: " Bin gleich wieder da. ".into(),
        };
        assert_eq!(phrase(&own, Some("de")), "Bin gleich wieder da.");
        assert_eq!(phrase(&own, None), "Bin gleich wieder da.");

        let tags: BTreeSet<&str> = PHRASES.iter().map(|(tag, _)| *tag).collect();
        assert_eq!(tags.len(), PHRASES.len());
    }

    #[test]
    fn each_input_makes_another_file() {
        let base = source("Einen Moment.");
        let name = base.file_name();
        assert_eq!(name, base.clone().file_name());
        assert_eq!(name.len(), "ack-".len() + 16 + ".mp3".len());
        assert!(name.starts_with("ack-") && name.ends_with(".mp3"));
        assert!(name[4..20].bytes().all(|b| b.is_ascii_hexdigit()));

        let others = [
            Source {
                preset: PresetId::ElevenLabs,
                ..base.clone()
            },
            Source {
                base_url: "http://tts.lan/v1".into(),
                ..base.clone()
            },
            Source {
                voice: "alloy".into(),
                ..base.clone()
            },
            Source {
                language: None,
                ..base.clone()
            },
            Source {
                model: "gpt-4o-mini-tts".into(),
                ..base.clone()
            },
            source("Moment."),
        ];
        let names: BTreeSet<String> = others.iter().map(Source::file_name).collect();
        assert_eq!(names.len(), others.len());
        assert!(!names.contains(&name));
        // Without a model of its own, the name is the one made before the
        // model could be chosen, so a cached phrase isn't synthesised again.
        assert_eq!(name, "ack-e0351b0e25a91b8f.mp3");
    }

    #[test]
    fn only_an_agent_with_a_checked_voice_wants_an_acknowledgment() {
        let config = |brain: PresetId| AssistConfig {
            connections: vec![
                Connection {
                    id: "speech".into(),
                    preset: PresetId::OpenAi,
                    base_url: "https://api.openai.com/v1".into(),
                    revision: 1,
                },
                Connection {
                    id: "brain".into(),
                    preset: brain,
                    base_url: "http://agent.lan:8642/v1".into(),
                    revision: 1,
                },
            ],
            stt: Some(SlotConfig {
                connection: "speech".into(),
                model: "whisper-1".into(),
                checked: 1,
                speech_model: None,
            }),
            chat: Some(SlotConfig {
                connection: "brain".into(),
                model: "gpt-test".into(),
                checked: 1,
                speech_model: None,
            }),
            tts: Some(SlotConfig {
                connection: "speech".into(),
                model: "nova".into(),
                checked: 1,
                speech_model: None,
            }),
            language: Some("de-DE".into()),
            ..AssistConfig::default()
        };
        let wanted = |config: &AssistConfig, tts_ready: bool| {
            let pipeline = Pipeline::build(config, &Secrets::default(), || true);
            Source::wanted(config, pipeline.as_ref(), tts_ready)
        };

        let agent = config(PresetId::Hermes);
        assert_eq!(
            wanted(&agent, true),
            Some(Source {
                preset: PresetId::OpenAi,
                base_url: "https://api.openai.com/v1".into(),
                voice: "nova".into(),
                model: String::new(),
                language: Some("de-DE".into()),
                phrase: "Einen Moment.".into(),
            })
        );
        assert_eq!(wanted(&agent, false), None);
        assert_eq!(wanted(&config(PresetId::OpenAi), true), None);
        let off = AssistConfig {
            ack: Ack {
                enabled: false,
                ..Ack::default()
            },
            ..agent.clone()
        };
        assert_eq!(wanted(&off, true), None);
        assert_eq!(Source::wanted(&agent, None, true), None);
    }

    #[tokio::test]
    async fn a_phrase_change_writes_a_new_file_and_keeps_the_previous_one() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path().join("cache"), Vec::new());
        let fake = Arc::new(FakeTts::default());
        let tts: Arc<dyn Tts> = fake.clone();
        let make = |source: Source| {
            let tts = tts.clone();
            move || Some((source.clone(), tts.clone()))
        };
        let (first, second, third) = (
            source("Einen Moment."),
            source("Moment."),
            source("Gleich."),
        );

        assert!(cache.refresh(make(first.clone())).await);
        let path = cache.path(&first).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"audio:Einen Moment.");
        assert_eq!(path, dir.path().join("cache").join(first.file_name()));

        // A phrase change writes a new file and keeps the one before.
        assert!(cache.refresh(make(second.clone())).await);
        assert_eq!(
            std::fs::read(cache.path(&second).unwrap()).unwrap(),
            b"audio:Moment."
        );
        assert!(cache.path(&first).is_some());

        // The one before that goes.
        assert!(cache.refresh(make(third.clone())).await);
        assert_eq!(
            files(&dir.path().join("cache")),
            BTreeSet::from([second.file_name(), third.file_name()])
        );
        assert_eq!(cache.path(&first), None);

        // A file on disk is not made again; nothing wanted makes nothing.
        assert!(!cache.refresh(make(third.clone())).await);
        assert!(!cache.refresh(|| None).await);
        assert_eq!(fake.said(), ["Einen Moment.", "Moment.", "Gleich."]);
    }

    #[tokio::test]
    async fn a_tts_failure_gives_no_acknowledgment_and_one_warning() {
        let dir = tempfile::tempdir().unwrap();
        // One retry, at once.
        let cache = AckCache::with(dir.path().to_owned(), vec![Duration::ZERO], |_| {
            REPORTED.fetch_add(1, Ordering::Relaxed);
        });
        let fake = Arc::new(FakeTts::failing(usize::MAX));
        let tts: Arc<dyn Tts> = fake.clone();
        let wanted = || Some((source("Einen Moment."), tts.clone()));

        assert!(!cache.refresh(wanted).await);
        assert!(!cache.refresh(wanted).await);

        assert_eq!(fake.said().len(), 4);
        assert_eq!(cache.path(&source("Einen Moment.")), None);
        assert!(files(dir.path()).is_empty());
        assert_eq!(REPORTED.load(Ordering::Relaxed), 1);

        // Audio too large to keep fails the same way.
        let large: Arc<dyn Tts> = Arc::new(FakeTts {
            size: Some(MAX_BYTES + 1),
            ..FakeTts::default()
        });
        assert!(
            !cache
                .refresh(|| Some((source("Moment."), large.clone())))
                .await
        );
        assert!(files(dir.path()).is_empty());
        assert_eq!(REPORTED.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn a_failed_synthesis_is_tried_again_while_it_is_wanted() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path().to_owned(), vec![Duration::ZERO; 2]);
        let fake = Arc::new(FakeTts::failing(2));
        let tts: Arc<dyn Tts> = fake.clone();

        assert!(
            cache
                .refresh(|| Some((source("Einen Moment."), tts.clone())))
                .await
        );

        assert_eq!(fake.said().len(), 3);
        assert!(cache.path(&source("Einen Moment.")).is_some());

        // A retry stops once nothing is wanted any more.
        let fake = Arc::new(FakeTts::failing(usize::MAX));
        let tts: Arc<dyn Tts> = fake.clone();
        let calls = AtomicUsize::new(0);
        let wanted = || {
            (calls.fetch_add(1, Ordering::Relaxed) == 0).then(|| (source("Moment."), tts.clone()))
        };
        assert!(!cache.refresh(wanted).await);
        assert_eq!(fake.said().len(), 1);
    }
}
