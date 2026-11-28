//! Which profile a spoken standalone request runs with (ADR 0038 §"Who the
//! conversation belongs to").
//!
//! A pure decision: what recognition has when speech-to-text returns, the
//! kind of request, the conversation's speaker and the owner's setting go in;
//! the profile, whether the conversation starts anew and who the conversation
//! belongs to from now on come out.

use super::Person;

/// What recognition had when speech-to-text returned.
pub enum Heard<'a> {
    /// A confident match, by id.
    Person(&'a str),
    /// No confident person, and why: "too late", "uncertain", "too short",
    /// "no model" or "recognition failed".
    Nobody(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Choice {
    /// The person whose profile applies; `None` is Everyone else.
    pub person: Option<Person>,
    /// Why, for the timeline and the log: "recognized", "carried over on a
    /// follow-up", "person removed" or why nobody was recognized.
    pub reason: &'static str,
    /// The conversation's speaker from now on, when this request sets it.
    /// An uncertain request never does.
    pub speaker: Option<String>,
    /// A speaker change, with "start a new conversation": forget the earlier
    /// turns first.
    pub reset: bool,
}

/// `conversation` is the id of the conversation's speaker, `lookup` finds a
/// person on the list now, and `keep` is the owner's "keep the conversation"
/// for a speaker change.
pub fn decide(
    heard: Heard<'_>,
    follow_up: bool,
    conversation: Option<&str>,
    keep: bool,
    lookup: impl Fn(&str) -> Option<Person>,
) -> Choice {
    match heard {
        Heard::Person(id) => match lookup(id) {
            // Removed since recognition ran: the request finishes as
            // Everyone else, and the conversation's speaker stays.
            None => Choice {
                person: None,
                reason: "person removed",
                speaker: None,
                reset: false,
            },
            Some(person) => {
                let reset = conversation.is_some_and(|speaker| speaker != id) && !keep;
                Choice {
                    speaker: Some(person.id.clone()),
                    person: Some(person),
                    reason: "recognized",
                    reset,
                }
            }
        },
        Heard::Nobody(why) => match conversation.filter(|_| follow_up) {
            Some(carried) => match lookup(carried) {
                Some(person) => Choice {
                    person: Some(person),
                    reason: "carried over on a follow-up",
                    speaker: None,
                    reset: false,
                },
                None => Choice {
                    person: None,
                    reason: "person removed",
                    speaker: None,
                    reset: false,
                },
            },
            None => Choice {
                person: None,
                reason: why,
                speaker: None,
                reset: false,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: &str) -> Person {
        Person {
            id: id.into(),
            name: id.to_uppercase(),
            instructions: String::new(),
            revision: 1,
        }
    }

    fn lookup(id: &str) -> Option<Person> {
        ["a", "b"].contains(&id).then(|| person(id))
    }

    fn names(choice: &Choice) -> Option<&str> {
        choice.person.as_ref().map(|p| p.id.as_str())
    }

    #[test]
    fn a_confident_person_gets_their_profile_in_a_request_and_a_follow_up() {
        for follow_up in [false, true] {
            let choice = decide(Heard::Person("a"), follow_up, None, false, lookup);
            assert_eq!(names(&choice), Some("a"));
            assert_eq!(choice.reason, "recognized");
            assert_eq!(choice.speaker.as_deref(), Some("a"));
            assert!(!choice.reset, "no speaker yet: nothing to reset");
        }
    }

    #[test]
    fn the_same_speaker_changes_nothing() {
        let choice = decide(Heard::Person("a"), true, Some("a"), false, lookup);
        assert_eq!(names(&choice), Some("a"));
        assert_eq!(choice.speaker.as_deref(), Some("a"));
        assert!(!choice.reset);
    }

    #[test]
    fn another_speaker_starts_a_new_conversation_unless_it_is_kept() {
        let choice = decide(Heard::Person("b"), false, Some("a"), false, lookup);
        assert_eq!(names(&choice), Some("b"));
        assert_eq!(choice.speaker.as_deref(), Some("b"));
        assert!(choice.reset);

        let choice = decide(Heard::Person("b"), false, Some("a"), true, lookup);
        assert_eq!(names(&choice), Some("b"));
        assert_eq!(
            choice.speaker.as_deref(),
            Some("b"),
            "the new person owns it"
        );
        assert!(!choice.reset);
    }

    #[test]
    fn an_uncertain_request_is_everyone_else_and_changes_nothing() {
        for why in ["uncertain", "too late", "too short", "no model"] {
            for conversation in [None, Some("a")] {
                let choice = decide(Heard::Nobody(why), false, conversation, false, lookup);
                assert_eq!(names(&choice), None);
                assert_eq!(choice.reason, why);
                assert_eq!(choice.speaker, None);
                assert!(!choice.reset);
            }
        }
    }

    #[test]
    fn an_uncertain_follow_up_carries_the_conversations_speaker_over() {
        let choice = decide(Heard::Nobody("uncertain"), true, Some("a"), false, lookup);
        assert_eq!(names(&choice), Some("a"));
        assert_eq!(choice.reason, "carried over on a follow-up");
        assert_eq!(choice.speaker, None);
        assert!(!choice.reset);

        // Nobody to carry over.
        let choice = decide(Heard::Nobody("too late"), true, None, false, lookup);
        assert_eq!(names(&choice), None);
        assert_eq!(choice.reason, "too late");
    }

    #[test]
    fn a_removed_person_is_everyone_else() {
        let choice = decide(Heard::Person("gone"), false, Some("a"), false, lookup);
        assert_eq!(names(&choice), None);
        assert_eq!(choice.reason, "person removed");
        assert_eq!(choice.speaker, None);
        assert!(!choice.reset);

        let choice = decide(
            Heard::Nobody("uncertain"),
            true,
            Some("gone"),
            false,
            lookup,
        );
        assert_eq!(names(&choice), None);
        assert_eq!(choice.reason, "person removed");
        assert_eq!(choice.speaker, None);
    }
}
