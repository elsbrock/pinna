//! The people the satellite recognizes by voice, and where their voiceprints
//! live (ADR 0038).
//!
//! The list is `people.toml` beside `settings.toml`: names and the extra
//! instructions each person gets, with a revision so two dashboard windows
//! don't overwrite each other. Voiceprints are separate files
//! ([`voiceprints`]). The speaker model loads in the background once someone
//! has a usable one ([`recognizer`]). Enrolling a voice is in [`enroll`].
//! Who speaks in each request is recognized by the [`tap`] and kept in the
//! [`Speaker`] slot.

pub mod enroll;
pub mod recognizer;
mod speaker;
pub mod tap;
pub mod voiceprints;

pub use self::speaker::Speaker;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use pinna_speaker::{Voiceprint, close};
use serde::{Deserialize, Serialize};

use self::recognizer::{Recognizer, State};
use self::voiceprints::{Stored, Voice};
use crate::standalone::service::Refusal;
use crate::store::Store;

pub const MAX_PEOPLE: usize = 10;
pub const MAX_NAME: usize = 40;
pub const MAX_INSTRUCTIONS: usize = 1_000;

/// The built-in profile's name, which no person may take.
const EVERYONE_ELSE: &str = "Everyone else";
const FILE_MODE: u32 = 0o600;

/// Where the files are: beside `settings.toml`.
#[derive(Clone, Debug)]
pub struct Paths {
    pub people: PathBuf,
    pub voiceprints: PathBuf,
}

impl Paths {
    pub fn beside(settings_file: &Path) -> Self {
        Self {
            people: settings_file.with_file_name("people.toml"),
            voiceprints: settings_file.with_file_name("voiceprints"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EveryoneElse {
    pub instructions: String,
    pub revision: u64,
}

impl Default for EveryoneElse {
    fn default() -> Self {
        Self {
            instructions: String::new(),
            revision: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    /// Assigned by pinnad (`p1`, `p2`, ...) and never reused.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub instructions: String,
    pub revision: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct File {
    everyone_else: EveryoneElse,
    people: Vec<Person>,
    /// The number the next person's id takes; it only grows, so an id isn't
    /// reused after its person was removed.
    next_id: u64,
}

pub struct PersonEdit {
    /// `None` adds a person.
    pub id: Option<String>,
    /// The revision the edit started from; ignored when adding.
    pub revision: u64,
    pub name: String,
    pub instructions: String,
}

/// A person with what the voiceprint files say.
#[derive(Debug, Clone)]
pub struct PersonView {
    pub person: Person,
    pub voice: Voice,
    /// The other people whose voiceprints are close to this one's.
    pub close_to: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct View {
    pub everyone_else: EveryoneElse,
    pub people: Vec<PersonView>,
    pub recognition: State,
}

struct Inner {
    store: Store<File>,
    value: File,
}

pub struct People {
    inner: Mutex<Inner>,
    voiceprints: PathBuf,
    recognizer: Recognizer,
    /// What was last logged for each voiceprint file that can't be read, so
    /// each reason is logged once.
    unreadable: Mutex<HashMap<String, String>>,
}

fn changed(name: &str) -> Refusal {
    Refusal::Conflict(format!(
        "Someone changed {name} since you opened it. Reload to see the change."
    ))
}

fn invalid(sentence: impl Into<String>) -> Refusal {
    Refusal::Invalid(sentence.into())
}

fn check_instructions(instructions: &str) -> Result<(), Refusal> {
    if instructions.chars().count() > MAX_INSTRUCTIONS {
        return Err(invalid(
            "Instructions can have at most 1,000 characters. Shorten them.",
        ));
    }
    Ok(())
}

impl People {
    /// Read the list, and start loading the speaker model if someone already
    /// has a voiceprint.
    pub fn open(paths: &Paths, speaker_model: PathBuf) -> Arc<Self> {
        let (store, value, _) = Store::load(&paths.people, FILE_MODE, File::default());
        let people = Arc::new(Self {
            inner: Mutex::new(Inner { store, value }),
            voiceprints: paths.voiceprints.clone(),
            recognizer: Recognizer::new(speaker_model),
            unreadable: Mutex::new(HashMap::new()),
        });
        people.sync_recognizer();
        people
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Each person's file, read now. A file that can't be read counts as
    /// none, and is logged once with the reason.
    fn read_all(&self, people: &[Person]) -> Vec<(String, Option<Stored>)> {
        let mut unreadable = self
            .unreadable
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        people
            .iter()
            .map(|p| {
                let stored = match voiceprints::read(&self.voiceprints, &p.id) {
                    Ok(stored) => {
                        unreadable.remove(&p.id);
                        stored
                    }
                    Err(e) => {
                        let reason = e.to_string();
                        if unreadable.insert(p.id.clone(), reason.clone()).as_ref() != Some(&reason)
                        {
                            tracing::warn!("{}'s voiceprint is ignored: {reason}", p.name);
                        }
                        None
                    }
                };
                (p.id.clone(), stored)
            })
            .collect()
    }

    /// Tell the recognizer which models the voiceprints were made with: it
    /// loads when one is usable and drops the model when none is. Called at
    /// start and after every change to the list or the files.
    fn sync_recognizer(&self) {
        let people = self.inner().value.people.clone();
        let shas = self
            .read_all(&people)
            .into_iter()
            .filter_map(|(_, stored)| stored.map(|s| s.model_sha))
            .collect();
        self.recognizer.set_wanted(shas);
    }

    pub fn view(&self) -> View {
        let value = self.inner().value.clone();
        let installed = self.recognizer.sha();
        let files = self.read_all(&value.people);
        let voices: Vec<(&Person, Voice, Option<&Voiceprint>)> = value
            .people
            .iter()
            .zip(&files)
            .map(|(p, (_, stored))| match stored {
                Some(s) => (
                    p,
                    voiceprints::classify(s, installed.as_deref()),
                    Some(&s.voiceprint),
                ),
                None => (p, Voice::None, None),
            })
            .collect();
        let people = voices
            .iter()
            .map(|(person, voice, mine)| {
                let close_to = match mine.filter(|_| voice.used()) {
                    Some(mine) => voices
                        .iter()
                        .filter(|(other, other_voice, _)| {
                            other.id != person.id && other_voice.used()
                        })
                        .filter(|(_, _, theirs)| theirs.is_some_and(|theirs| close(mine, theirs)))
                        .map(|(other, ..)| other.id.clone())
                        .collect(),
                    None => Vec::new(),
                };
                PersonView {
                    person: (*person).clone(),
                    voice: *voice,
                    close_to,
                }
            })
            .collect();
        View {
            everyone_else: value.everyone_else,
            people,
            recognition: self.recognizer.state(),
        }
    }

    /// The voiceprints matching uses, as (id, name, voiceprint): those made by
    /// the installed model. Reads the files; call it off the capture loop.
    pub fn usable_voiceprints(&self) -> Vec<(String, String, Voiceprint)> {
        let people = self.inner().value.people.clone();
        let installed = self.recognizer.sha();
        self.read_all(&people)
            .into_iter()
            .zip(people)
            .filter_map(|((_, stored), person)| {
                let stored = stored?;
                voiceprints::classify(&stored, installed.as_deref())
                    .used()
                    .then_some((person.id, person.name, stored.voiceprint))
            })
            .collect()
    }

    /// The view of one person, after a save.
    pub fn person(&self, id: &str) -> Option<PersonView> {
        self.view().people.into_iter().find(|p| p.person.id == id)
    }

    fn save(inner: &mut Inner, next: File) -> Result<(), Refusal> {
        inner.store.try_save(&next).map_err(|e| {
            Refusal::Precondition(format!(
                "Pinna couldn't save the people: {e}. Check the satellite's storage and try again."
            ))
        })?;
        inner.value = next;
        Ok(())
    }

    /// Add a person, or change one. Returns the person as saved.
    pub fn set_person(&self, edit: PersonEdit) -> Result<Person, Refusal> {
        let name = edit.name.trim().to_owned();
        if name.is_empty() || name.chars().count() > MAX_NAME {
            return Err(invalid(
                "A name has 1 to 40 characters. Enter a shorter one.",
            ));
        }
        if name.to_lowercase() == EVERYONE_ELSE.to_lowercase() {
            return Err(invalid(
                "Everyone else is the profile for anyone who isn't on the list. Choose another name.",
            ));
        }
        check_instructions(&edit.instructions)?;
        let mut inner = self.inner();
        let mut next = inner.value.clone();
        if let Some(other) = next.people.iter().find(|p| {
            p.name.to_lowercase() == name.to_lowercase() && Some(&p.id) != edit.id.as_ref()
        }) {
            return Err(invalid(format!(
                "{} is on the list already. Choose another name.",
                other.name
            )));
        }
        let saved = match &edit.id {
            None => {
                if next.people.len() >= MAX_PEOPLE {
                    return Err(invalid(
                        "The list holds 10 people. Remove someone before you add another.",
                    ));
                }
                let id = format!("p{}", next.next_id.max(1));
                next.next_id = next.next_id.max(1) + 1;
                let person = Person {
                    id,
                    name,
                    instructions: edit.instructions,
                    revision: 1,
                };
                next.people.push(person.clone());
                person
            }
            Some(id) => {
                let Some(current) = next.people.iter_mut().find(|p| &p.id == id) else {
                    return Err(Refusal::Conflict(
                        "This person was removed meanwhile. Reload to see the change.".into(),
                    ));
                };
                if current.revision != edit.revision {
                    return Err(changed(&current.name));
                }
                if current.name == name && current.instructions == edit.instructions {
                    return Ok(current.clone());
                }
                current.name = name;
                current.instructions = edit.instructions;
                current.revision += 1;
                current.clone()
            }
        };
        Self::save(&mut inner, next)?;
        Ok(saved)
    }

    /// Remove a person and their voiceprint. Someone who isn't on the list
    /// is already gone.
    pub fn delete_person(&self, id: &str) -> Result<(), Refusal> {
        {
            let mut inner = self.inner();
            if !inner.value.people.iter().any(|p| p.id == id) {
                return Ok(());
            }
            let mut next = inner.value.clone();
            next.people.retain(|p| p.id != id);
            Self::save(&mut inner, next)?;
        }
        if let Err(e) = voiceprints::remove(&self.voiceprints, id) {
            tracing::warn!("{e}");
        }
        self.unreadable
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        self.sync_recognizer();
        Ok(())
    }

    pub fn set_everyone_else(
        &self,
        instructions: String,
        revision: u64,
    ) -> Result<EveryoneElse, Refusal> {
        check_instructions(&instructions)?;
        let mut inner = self.inner();
        if inner.value.everyone_else.revision != revision {
            return Err(changed(EVERYONE_ELSE));
        }
        if inner.value.everyone_else.instructions == instructions {
            return Ok(inner.value.everyone_else.clone());
        }
        let mut next = inner.value.clone();
        next.everyone_else.instructions = instructions;
        next.everyone_else.revision += 1;
        Self::save(&mut inner, next)?;
        Ok(inner.value.everyone_else.clone())
    }

    /// The speaker model's lifecycle and thread, for enrollment.
    pub fn recognizer(&self) -> &Recognizer {
        &self.recognizer
    }

    /// Keep a person's voiceprint, made by the model with `model_sha`
    /// (lowercase hex). For enrollment.
    pub fn save_voiceprint(
        &self,
        id: &str,
        voiceprint: &Voiceprint,
        model_sha: &str,
    ) -> Result<(), voiceprints::Error> {
        if !self.inner().value.people.iter().any(|p| p.id == id) {
            return Err(voiceprints::Error::Invalid(
                "belongs to someone who isn't on the list",
            ));
        }
        voiceprints::write(&self.voiceprints, id, voiceprint, model_sha)?;
        self.sync_recognizer();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::voiceprints::synthetic;
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    struct Rig {
        dir: tempfile::TempDir,
        people: Arc<People>,
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let people = Self::open(dir.path());
            Self { dir, people }
        }

        /// The files are beside `settings.toml`, which a test needn't write.
        fn open(dir: &Path) -> Arc<People> {
            People::open(
                &Paths::beside(&dir.join("settings.toml")),
                "/nonexistent/speaker.onnx".into(),
            )
        }

        fn add(&self, name: &str) -> Result<Person, Refusal> {
            self.people.set_person(PersonEdit {
                id: None,
                revision: 0,
                name: name.into(),
                instructions: String::new(),
            })
        }

        fn vp_file(&self, id: &str) -> PathBuf {
            voiceprints::path(&self.dir.path().join("voiceprints"), id)
        }
    }

    fn sentence(r: Result<impl std::fmt::Debug, Refusal>) -> String {
        r.unwrap_err().sentence().to_owned()
    }

    #[test]
    fn a_person_starts_at_revision_one_and_each_edit_adds_one() {
        let rig = Rig::new();
        let anna = rig.add("  Anna ").unwrap();
        assert_eq!(
            (anna.id.as_str(), anna.name.as_str(), anna.revision),
            ("p1", "Anna", 1)
        );
        let edited = rig
            .people
            .set_person(PersonEdit {
                id: Some("p1".into()),
                revision: 1,
                name: "Anna".into(),
                instructions: "Short answers.".into(),
            })
            .unwrap();
        assert_eq!(edited.revision, 2);
        let text = std::fs::read_to_string(rig.dir.path().join("people.toml")).unwrap();
        assert!(text.contains("Short answers."), "{text}");
        let mode = std::fs::metadata(rig.dir.path().join("people.toml")).unwrap();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777,
            0o600
        );
    }

    #[test]
    fn the_list_survives_a_restart() {
        let rig = Rig::new();
        rig.add("Anna").unwrap();
        let again = Rig::open(rig.dir.path());
        assert_eq!(again.view().people[0].person.name, "Anna");
    }

    #[test]
    fn a_name_has_one_to_forty_characters() {
        let rig = Rig::new();
        assert!(sentence(rig.add("   ")).contains("1 to 40"));
        assert!(sentence(rig.add(&"a".repeat(41))).contains("1 to 40"));
        rig.add(&"ä".repeat(40)).unwrap();
    }

    #[test]
    fn names_are_unique_ignoring_case_and_never_everyone_else() {
        let rig = Rig::new();
        rig.add("Anna").unwrap();
        assert!(sentence(rig.add("ANNA")).contains("on the list already"));
        assert!(sentence(rig.add("everyone ELSE")).contains("Choose another name"));
        // Changing the case of one's own name is fine.
        rig.people
            .set_person(PersonEdit {
                id: Some("p1".into()),
                revision: 1,
                name: "ANNA".into(),
                instructions: String::new(),
            })
            .unwrap();
    }

    #[test]
    fn instructions_have_at_most_a_thousand_characters() {
        let rig = Rig::new();
        let edit = |instructions: String| PersonEdit {
            id: None,
            revision: 0,
            name: "Anna".into(),
            instructions,
        };
        assert!(sentence(rig.people.set_person(edit("x".repeat(1_001)))).contains("1,000"));
        rig.people.set_person(edit("x".repeat(1_000))).unwrap();
        assert!(sentence(rig.people.set_everyone_else("x".repeat(1_001), 1)).contains("1,000"));
    }

    #[test]
    fn at_most_ten_people() {
        let rig = Rig::new();
        for n in 0..10 {
            rig.add(&format!("Person {n}")).unwrap();
        }
        assert!(sentence(rig.add("One more")).contains("10 people"));
    }

    #[test]
    fn a_stale_edit_is_refused_with_the_name() {
        let rig = Rig::new();
        rig.add("Anna").unwrap();
        let edit = |revision| PersonEdit {
            id: Some("p1".into()),
            revision,
            name: "Anna".into(),
            instructions: "Changed.".into(),
        };
        rig.people.set_person(edit(1)).unwrap();
        let refused = rig.people.set_person(edit(1)).unwrap_err();
        assert!(matches!(refused, Refusal::Conflict(_)));
        assert_eq!(
            refused.sentence(),
            "Someone changed Anna since you opened it. Reload to see the change."
        );
        assert!(matches!(
            rig.people.set_everyone_else("x".into(), 7),
            Err(Refusal::Conflict(_))
        ));
        assert_eq!(
            rig.people
                .set_everyone_else("x".into(), 1)
                .unwrap()
                .revision,
            2
        );
    }

    #[test]
    fn an_id_is_not_reused_after_its_person_is_removed() {
        let rig = Rig::new();
        rig.add("Anna").unwrap();
        rig.add("Ben").unwrap();
        rig.people.delete_person("p2").unwrap();
        assert_eq!(rig.add("Cleo").unwrap().id, "p3");
        let again = Rig::open(rig.dir.path());
        assert_eq!(
            again
                .set_person(PersonEdit {
                    id: None,
                    revision: 0,
                    name: "Dan".into(),
                    instructions: String::new(),
                })
                .unwrap()
                .id,
            "p4"
        );
    }

    #[test]
    fn removing_a_person_deletes_their_voiceprint() {
        let rig = Rig::new();
        rig.add("Anna").unwrap();
        rig.people
            .save_voiceprint("p1", &synthetic(&[1.0, 0.0]), SHA)
            .unwrap();
        assert!(rig.vp_file("p1").exists());
        rig.people.delete_person("p1").unwrap();
        assert!(!rig.vp_file("p1").exists());
        rig.people.delete_person("p1").unwrap();
    }

    #[test]
    fn a_voiceprint_needs_a_person_on_the_list() {
        let rig = Rig::new();
        assert!(
            rig.people
                .save_voiceprint("p1", &synthetic(&[1.0]), SHA)
                .is_err()
        );
        assert!(!rig.vp_file("p1").exists());
    }

    #[test]
    fn voice_states_follow_the_files() {
        let rig = Rig::new();
        rig.add("Anna").unwrap();
        rig.add("Ben").unwrap();
        rig.people
            .save_voiceprint("p1", &synthetic(&[1.0, 0.0]), SHA)
            .unwrap();
        std::fs::write(rig.vp_file("p2"), b"garbage").unwrap();
        let view = rig.people.view();
        assert_eq!(view.people[0].voice, Voice::Enrolled);
        assert_eq!(
            view.people[1].voice,
            Voice::None,
            "an unreadable file is none"
        );
    }

    #[test]
    fn close_pairs_name_each_other_and_skip_unused_voiceprints() {
        let rig = Rig::new();
        for name in ["Anna", "Ben", "Cleo", "Dan"] {
            rig.add(name).unwrap();
        }
        let save = |id, v: &[f32]| rig.people.save_voiceprint(id, &synthetic(v), SHA).unwrap();
        save("p1", &[1.0, 0.0, 0.0]);
        save("p2", &[1.0, 0.2, 0.0]);
        save("p3", &[0.0, 0.0, 1.0]);
        // Identical to Anna's.
        save("p4", &[1.0, 0.0, 0.0]);
        let view = rig.people.view();
        assert_eq!(view.people[0].close_to, ["p2", "p4"]);
        assert_eq!(view.people[1].close_to, ["p1", "p4"]);
        assert!(view.people[2].close_to.is_empty());
    }

    #[test]
    fn recognition_is_not_needed_until_someone_has_a_voiceprint() {
        let rig = Rig::new();
        rig.add("Anna").unwrap();
        assert_eq!(rig.people.view().recognition, State::NotNeeded);
        rig.people
            .save_voiceprint("p1", &synthetic(&[1.0, 0.0]), SHA)
            .unwrap();
        // The model file is missing, so loading fails with the reason.
        let mut state = rig.people.view().recognition;
        for _ in 0..100 {
            if !matches!(state, State::Loading) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            state = rig.people.view().recognition;
        }
        assert!(matches!(state, State::Unavailable(_)), "{state:?}");
        rig.people.delete_person("p1").unwrap();
        assert_eq!(rig.people.view().recognition, State::NotNeeded);
    }
}
