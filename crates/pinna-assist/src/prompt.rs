//! The system prompt: the voice rules, the owner's text and the context block
//! sent with every request (ADR 0017 §"The prompt is the owner's").
//!
//! The prompt is spoken, so the rules keep replies short and plain. The context
//! is appended whatever the owner wrote, and models and agents get the same
//! text.

/// The owner's prompt is edited in full, at most this many characters.
pub const PROMPT_MAX: usize = 4_000;

/// pinnad's voice rules, the starting prompt. A reset brings them back. The
/// last sentence is for models that answer "timer set" without calling the
/// tool, which the owner can't tell from a spoken reply.
pub const VOICE_RULES: &str = "You are a voice assistant on a small speaker. Answer in speech: short, \
plain text, with no markdown, lists or links. Write numbers the way they are said aloud. Ask a \
question only when you need an answer. Do things only by calling your tools, and never say you did \
something that no tool did; if no tool can do it, say so.";

/// What pinnad appends after the owner's prompt with each request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// The local time as it is spoken, e.g. `Tuesday 17 November 2026, 14:03`.
    pub time: String,
    /// The IANA zone name, e.g. `Europe/Berlin`. Empty with the time is UTC.
    pub zone: String,
    /// The place set on the dashboard, if any.
    pub place: Option<String>,
    /// The language to answer in, if set.
    pub language: Option<String>,
    /// The satellite's name.
    pub satellite: String,
    /// The other members of the satellite's group (ADR 0026); empty in no
    /// group.
    pub group: Vec<GroupMember>,
}

/// Another satellite of the group, as the brain hears of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMember {
    pub name: String,
    /// `None` until the member was heard from.
    pub mode: Option<MemberMode>,
}

/// A member's mode: the group works in standalone mode only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberMode {
    Standalone,
    /// Out of reach: the brain says so when asked to reach it.
    HomeAssistant,
}

/// The system prompt for one request. `owner` is the stored prompt:
/// `None` gives [`VOICE_RULES`], an empty string gives the context only, and
/// anything else comes first, with the context after it.
pub fn system(owner: Option<&str>, ctx: &Context) -> String {
    let mut out = String::new();
    match owner {
        None => out.push_str(VOICE_RULES),
        Some("") => {}
        Some(text) => out.push_str(text.trim()),
    }
    let block = context_block(ctx);
    if !block.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&block);
    }
    out
}

/// The context lines. `None` fields are left out.
fn context_block(ctx: &Context) -> String {
    let mut lines: Vec<String> = Vec::new();
    if !ctx.time.is_empty() {
        if ctx.zone.is_empty() {
            lines.push(format!("Local time: {}.", ctx.time));
        } else {
            lines.push(format!("Local time: {} ({}).", ctx.time, ctx.zone));
        }
    }
    if let Some(place) = &ctx.place
        && !place.is_empty()
    {
        lines.push(format!("Place: {place}."));
    }
    if let Some(language) = &ctx.language
        && !language.is_empty()
    {
        lines.push(format!("Language: {language}."));
    }
    if !ctx.satellite.is_empty() {
        lines.push(format!("Satellite: {}.", ctx.satellite));
    }
    if !ctx.group.is_empty() {
        let members: Vec<String> = ctx
            .group
            .iter()
            .map(|m| match m.mode {
                Some(MemberMode::Standalone) => format!("{} (standalone mode)", m.name),
                Some(MemberMode::HomeAssistant) => {
                    format!(
                        "{} (Home Assistant mode: takes messages and control, not timers)",
                        m.name
                    )
                }
                None => m.name.clone(),
            })
            .collect();
        lines.push(format!(
            "Other satellites in its group: {}.",
            members.join(", ")
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> Context {
        Context {
            time: "Tuesday 17 November 2026, 14:03".into(),
            zone: "Europe/Berlin".into(),
            place: Some("Munich".into()),
            language: Some("de".into()),
            satellite: "Kitchen Dot".into(),
            group: Vec::new(),
        }
    }

    #[test]
    fn default_prompt_uses_the_voice_rules() {
        let prompt = system(None, &full());
        assert!(prompt.starts_with(VOICE_RULES));
        assert!(prompt.contains("Local time: Tuesday 17 November 2026, 14:03 (Europe/Berlin)."));
        assert!(prompt.contains("Place: Munich."));
        assert!(prompt.contains("Language: de."));
        assert!(prompt.contains("Satellite: Kitchen Dot."));
    }

    #[test]
    fn empty_prompt_is_only_the_context() {
        let prompt = system(Some(""), &full());
        assert!(!prompt.contains(VOICE_RULES));
        assert!(prompt.starts_with("Local time:"));
    }

    #[test]
    fn custom_prompt_comes_first() {
        let prompt = system(Some("  Be brief.  "), &full());
        assert!(prompt.starts_with("Be brief.\n\nLocal time:"));
        assert!(!prompt.contains(VOICE_RULES));
    }

    #[test]
    fn context_lines_are_omitted_when_unset() {
        let ctx = Context {
            time: "14:03".into(),
            zone: "UTC".into(),
            place: None,
            language: None,
            satellite: "Dot".into(),
            group: Vec::new(),
        };
        let prompt = system(None, &ctx);
        assert!(prompt.contains("Local time: 14:03 (UTC)."));
        assert!(prompt.contains("Satellite: Dot."));
        assert!(!prompt.contains("Place:"));
        assert!(!prompt.contains("Language:"));
        assert!(!prompt.contains("group"));
    }

    #[test]
    fn the_group_names_each_member_with_its_mode_when_known() {
        let member = |name: &str, mode| GroupMember {
            name: name.into(),
            mode,
        };
        let ctx = Context {
            group: vec![
                member("Kitchen", Some(MemberMode::Standalone)),
                member("Office", None),
                member("Porch", Some(MemberMode::HomeAssistant)),
            ],
            ..full()
        };
        let prompt = system(None, &ctx);
        assert!(
            prompt.ends_with(
                "Satellite: Kitchen Dot.\nOther satellites in its group: Kitchen (standalone \
                 mode), Office, Porch (Home Assistant mode: takes messages and control, not timers)."
            ),
            "{prompt}"
        );
    }

    #[test]
    fn empty_context_is_empty() {
        assert_eq!(system(Some(""), &Context::default()), "");
        assert_eq!(system(None, &Context::default()), VOICE_RULES);
    }

    #[test]
    fn missing_zone_is_just_the_time() {
        let ctx = Context {
            time: "14:03".into(),
            ..Context::default()
        };
        assert_eq!(system(Some(""), &ctx), "Local time: 14:03.");
    }
}
