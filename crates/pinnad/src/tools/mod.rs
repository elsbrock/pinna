//! The tools the brain and MCP clients call (ADR 0017).
//!
//! One table names every tool, the surfaces that offer it and its
//! arguments. The owner switches tools off per name in standalone.toml; a
//! tool that is off is left out of the list and refused when called anyway.
//!
//! A tool-loop brain also gets the tools of the owner's MCP servers, as
//! `<server>_<tool>`. A device tool's name is never lent to one of them.
//!
//! In a satellite group (ADR 0026), the brain's tools that act on the
//! satellite take another member's name as `satellite`, and that member
//! runs them. `/mcp`'s tools stay as they are.

mod device;
mod http;
pub mod message;
pub mod radio;
mod remote;
mod update;
pub mod weather;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::{BoxFuture, join_all};
use pinna_assist::presets::Delegation;
use pinna_mcp::{ToolOutput, ToolSet, ToolSpec};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot, watch};

use crate::group::Group;
use crate::leds::LightEffect;
use crate::metrics::{Counters, Via};
use crate::standalone::{AssistLive, OwnerMcp};
use crate::updates::Updater;
use crate::voice::{ApiCommand, Input, RunId, Snapshot};
use device::Op;
use radio::RadioBrowser;
use remote::Target;
use weather::OpenMeteo;

/// Free Brain queue slots an MCP call, or a message from the satellite
/// group, leaves for the wake word and buttons.
pub(crate) const BRAIN_RESERVE: usize = 16;
pub const BUSY: &str = "The satellite is busy. Try again in a moment.";
/// The argument that names another member of the satellite group
/// (ADR 0026).
pub const SATELLITE: &str = "satellite";
const SHUTTING_DOWN: &str = "pinnad is shutting down.";

/// Where a tool list is offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// A standalone run's tool loop.
    Run,
    /// Pinna's MCP server.
    Mcp,
}

/// Who calls a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    /// A standalone run: the Brain applies the command only while the run
    /// is current.
    Run(RunId),
    /// An MCP client.
    Mcp,
    /// Another member of the satellite group (ADR 0026). Its calls run as
    /// an MCP client's do.
    Peer,
}

/// The surfaces that offer a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    Both,
    RunOnly,
    McpOnly,
}

impl Reach {
    fn offers(self, surface: Surface) -> bool {
        matches!(
            (self, surface),
            (Reach::Both, _) | (Reach::RunOnly, Surface::Run) | (Reach::McpOnly, Surface::Mcp)
        )
    }
}

/// Whether a tool acts on another member of the satellite group
/// (ADR 0026): the brain names the member, and the member runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Remote {
    No,
    /// On this satellite or on another member.
    Also,
    /// On another member only, with what it does there as the brain sees
    /// it.
    Only(&'static str),
}

/// `satellite` as the brain sees it, for a tool it may run here too.
const ANOTHER: &str =
    "Another satellite of this one's group to do it on, by its name. Leave it out for this one.";
/// `satellite` for a tool the brain runs on another member only.
const WHICH: &str = "The other satellite of this one's group, by its name.";
/// What the dashboard adds to the description of a tool the brain runs on
/// another member only, so no owner reads it as the brain's on this one.
const ELSEWHERE: &str = "The brain does this only on another satellite of its group.";

/// An argument's JSON type. Bounds and words are hints for the caller; the
/// tool's checks decide.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Integer { min: i64, max: i64 },
    Text,
    Boolean,
    Choice(&'static [&'static str]),
}

impl Kind {
    fn accepts(self, value: &Value) -> bool {
        match self {
            Kind::Integer { .. } => whole(value).is_some(),
            Kind::Text | Kind::Choice(_) => value.is_string(),
            Kind::Boolean => value.is_boolean(),
        }
    }

    fn noun(self) -> String {
        match self {
            Kind::Integer { .. } => "a whole number".into(),
            Kind::Text => "text".into(),
            Kind::Boolean => "true or false".into(),
            Kind::Choice(words) => format!("one of {}", words.join(", ")),
        }
    }

    fn schema(self) -> Value {
        match self {
            Kind::Integer { min, max } => {
                json!({"type": "integer", "minimum": min, "maximum": max})
            }
            Kind::Text => json!({"type": "string"}),
            Kind::Boolean => json!({"type": "boolean"}),
            Kind::Choice(words) => json!({"type": "string", "enum": words}),
        }
    }
}

/// A JSON number without a fraction, such as `40` or `40.0`.
fn whole(value: &Value) -> Option<i64> {
    // Beyond 2^53 a float no longer holds every whole number.
    const EXACT: f64 = 9_007_199_254_740_992.0;
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.fract() == 0.0 && f.abs() <= EXACT)
            .map(|f| f as i64)
    })
}

struct Param {
    name: &'static str,
    kind: Kind,
    required: bool,
    about: &'static str,
}

struct Tool {
    name: &'static str,
    reach: Reach,
    /// The tool is off until the owner turns it on; every other tool is on
    /// until the owner turns it off.
    off_by_default: bool,
    remote: Remote,
    about: &'static str,
    params: &'static [Param],
    op: Op,
}

impl Tool {
    fn spec(&self) -> ToolSpec {
        let properties: Map<String, Value> = self
            .params
            .iter()
            .map(|p| {
                let mut schema = p.kind.schema();
                schema["description"] = p.about.into();
                (p.name.to_owned(), schema)
            })
            .collect();
        let required: Vec<&str> = self
            .params
            .iter()
            .filter(|p| p.required)
            .map(|p| p.name)
            .collect();
        ToolSpec {
            name: self.name.into(),
            description: self.about.into(),
            input_schema: json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            }),
        }
    }

    /// The tool as a brain in a satellite group sees it (ADR 0026): one
    /// that acts on the satellite takes `satellite`, which one offered for
    /// another member only needs.
    fn grouped_spec(&self) -> ToolSpec {
        let mut spec = self.spec();
        let about = match self.remote {
            Remote::No => return spec,
            Remote::Also => ANOTHER,
            Remote::Only(there) => {
                spec.description = there.into();
                if let Some(required) = spec.input_schema["required"].as_array_mut() {
                    required.push(SATELLITE.into());
                }
                WHICH
            }
        };
        spec.input_schema["properties"][SATELLITE] =
            json!({"type": "string", "description": about});
        spec
    }
}

const PERCENT: Kind = Kind::Integer { min: 0, max: 100 };

static TOOLS: &[Tool] = &[
    Tool {
        name: "set_volume",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "Set the speaker volume.",
        params: &[Param {
            name: "percent",
            kind: PERCENT,
            required: true,
            about: "The volume, from 0 to 100.",
        }],
        op: Op::SetVolume,
    },
    Tool {
        name: "change_volume",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "Make the speaker louder or quieter.",
        params: &[Param {
            name: "by",
            kind: Kind::Integer {
                min: -100,
                max: 100,
            },
            required: true,
            about: "Percentage points to add; negative makes it quieter.",
        }],
        op: Op::ChangeVolume,
    },
    Tool {
        name: "set_reply_volume",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "Set how loud spoken replies play. A reply is never louder than the speaker volume.",
        params: &[Param {
            name: "percent",
            kind: PERCENT,
            required: true,
            about: "The reply volume, from 0 to 100.",
        }],
        op: Op::SetReplyVolume,
    },
    Tool {
        name: "ring_light",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "Switch the LED ring light on or off, or change its brightness, colour or effect.",
        params: &[
            Param {
                name: "on",
                kind: Kind::Boolean,
                required: false,
                about: "Whether the light is on.",
            },
            Param {
                name: "brightness",
                kind: PERCENT,
                required: false,
                about: "Brightness, from 0 to 100.",
            },
            Param {
                name: "color",
                kind: Kind::Text,
                required: false,
                about: "A hex colour such as #ff8800.",
            },
            Param {
                name: "effect",
                kind: Kind::Choice(&LightEffect::NAMES),
                required: false,
                about: "How the light shows.",
            },
        ],
        op: Op::RingLight,
    },
    Tool {
        name: "play_url",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "Play audio from an http or https address, such as an internet radio stream.",
        params: &[Param {
            name: "url",
            kind: Kind::Text,
            required: true,
            about: "The address of the audio.",
        }],
        op: Op::PlayUrl,
    },
    Tool {
        name: "pause_media",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "Pause the music or radio.",
        params: &[],
        op: Op::PauseMedia,
    },
    Tool {
        name: "resume_media",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "Resume paused music or radio.",
        params: &[],
        op: Op::ResumeMedia,
    },
    Tool {
        name: "stop_media",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "Stop the music or radio.",
        params: &[],
        op: Op::StopMedia,
    },
    Tool {
        name: "play_station",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "Play one of the owner's radio stations, found by its name.",
        params: &[Param {
            name: "name",
            kind: Kind::Text,
            required: true,
            about: "The station's name, or part of it.",
        }],
        op: Op::PlayStation,
    },
    Tool {
        name: "list_stations",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "List the names of the owner's radio stations.",
        params: &[],
        op: Op::ListStations,
    },
    Tool {
        name: "search_radio",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::No,
        about: "Search the radio-browser.info directory for internet radio stations this satellite can play. Each result has the address that plays it.",
        params: &[Param {
            name: "name",
            kind: Kind::Text,
            required: true,
            about: "The station's name, or part of it.",
        }],
        op: Op::SearchRadio,
    },
    Tool {
        name: "clear_notification",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::No,
        about: "Clear the notification the light ring shows.",
        params: &[],
        op: Op::ClearNotification,
    },
    Tool {
        name: "mute_microphone",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::No,
        about: "Mute the microphone. Only a person can unmute it.",
        params: &[],
        op: Op::MuteMicrophone,
    },
    Tool {
        name: "switch_assistant",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::No,
        about: "Switch the satellite between Home Assistant and standalone mode. It switches once the satellite is idle, so the reply that confirms it still plays. Standalone is refused until speech-to-text, the brain and text-to-speech are set up.",
        params: &[Param {
            name: "mode",
            kind: Kind::Choice(&["home_assistant", "standalone"]),
            required: true,
            about: "The mode to switch to.",
        }],
        op: Op::SwitchAssistant,
    },
    Tool {
        name: "updates",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::No,
        about: "The software version the satellite runs and the update state: the other slot, the update channel, what the last check found or why it failed, and the install that runs or the last one.",
        params: &[],
        op: Op::Updates,
    },
    Tool {
        name: "check_for_updates",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::No,
        about: "Check now for a new software release. Answers with what the check found, or says the check goes on when it takes longer than a few seconds. Refused while updates are off.",
        params: &[],
        op: Op::CheckForUpdates,
    },
    Tool {
        name: "install_update",
        reach: Reach::RunOnly,
        // It ends in a restart, and a model can call a tool nobody asked for.
        off_by_default: true,
        remote: Remote::No,
        about: "Install the software release the last check found. Use it only when the person asks to install the update. It downloads in the background and the satellite restarts once it is idle. It never installs an older version.",
        params: &[Param {
            name: "version",
            kind: Kind::Text,
            required: true,
            about: "The release's version, as the updates tool shows it.",
        }],
        op: Op::InstallUpdate,
    },
    Tool {
        name: "set_timer",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "Start a timer that rings on this satellite.",
        params: &[
            Param {
                name: "seconds",
                kind: Kind::Integer {
                    min: 1,
                    max: crate::checks::TIMER_SECONDS_MAX as i64,
                },
                required: true,
                about: "How long the timer runs, in seconds.",
            },
            Param {
                name: "name",
                kind: Kind::Text,
                required: false,
                about: "A short name, such as pasta.",
            },
        ],
        op: Op::SetTimer,
    },
    Tool {
        name: "list_timers",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "List the timers with the seconds each has left.",
        params: &[],
        op: Op::ListTimers,
    },
    Tool {
        name: "cancel_timer",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::Also,
        about: "Cancel a timer.",
        params: &[Param {
            name: "timer",
            kind: Kind::Text,
            required: true,
            about: "The timer's id or name.",
        }],
        op: Op::CancelTimer,
    },
    Tool {
        name: "time",
        reach: Reach::Both,
        off_by_default: false,
        remote: Remote::No,
        about: "The current local date and time where the satellite is.",
        params: &[],
        op: Op::Time,
    },
    Tool {
        name: weather::NAME,
        reach: Reach::RunOnly,
        off_by_default: false,
        remote: Remote::No,
        about: "The weather now and for the next two days, at a place the person names, or else at the satellite's place.",
        params: &[Param {
            name: "place",
            kind: Kind::Text,
            required: false,
            about: "A town or city, with its country after a comma when the name is ambiguous, such as Paris, France. Leave it out for the satellite's own place.",
        }],
        op: Op::Weather,
    },
    Tool {
        name: message::NAME,
        reach: Reach::RunOnly,
        off_by_default: false,
        remote: Remote::No,
        about: "Send a message to another satellite of this one's group, or to all of them, to be said there in its voice. It plays at once, or waits until that satellite is free. The result says, for each, whether it played, waits, was refused and why, or couldn't be reached.",
        params: &[
            Param {
                name: "to",
                kind: Kind::Text,
                required: true,
                about: "The satellite's name, or all for every other satellite of the group.",
            },
            Param {
                name: "text",
                kind: Kind::Text,
                required: true,
                about: "What to say, at most 500 characters.",
            },
        ],
        op: Op::SendMessage,
    },
    Tool {
        name: "speak",
        reach: Reach::McpOnly,
        off_by_default: false,
        remote: Remote::No,
        about: "Say a short text aloud with the satellite's voice. Refused while it handles a request, an alarm rings or it plays something else.",
        params: &[Param {
            name: "text",
            kind: Kind::Text,
            required: true,
            about: "What to say, at most 500 characters.",
        }],
        op: Op::Speak,
    },
    Tool {
        name: "cancel_request",
        reach: Reach::McpOnly,
        off_by_default: false,
        remote: Remote::Only(
            "Stop the request another satellite of the group is handling, silence its ringing alarm, or stop a message or speak it is saying.",
        ),
        about: "Stop the request the satellite is handling, or silence a ringing alarm.",
        params: &[],
        op: Op::CancelRequest,
    },
    Tool {
        name: "status",
        reach: Reach::McpOnly,
        off_by_default: false,
        remote: Remote::No,
        about: "The satellite's mode, phase, volume, microphone, media, timers and alarm.",
        params: &[],
        op: Op::Status,
    },
];

/// The registry's own spelling of a device tool's name, for metric labels;
/// `None` for any other name.
pub fn device_tool_name(name: &str) -> Option<&'static str> {
    TOOLS.iter().map(|t| t.name).find(|t| *t == name)
}

/// Whether a member runs `name` when another member of the satellite group
/// asks (ADR 0026): the tools that act on the satellite, `cancel_request`
/// among them.
pub fn runs_for_group(name: &str) -> bool {
    TOOLS
        .iter()
        .any(|t| t.name == name && t.remote != Remote::No)
}

/// A device tool as the dashboard lists it beside its switch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolInfo {
    pub name: &'static str,
    pub description: String,
    /// The brain's tool loop offers it.
    pub brain: bool,
    /// `/mcp` offers it.
    pub mcp: bool,
    /// Off until the owner turns it on.
    pub off_by_default: bool,
}

/// Every device tool, in registry order.
pub fn catalogue() -> impl Iterator<Item = ToolInfo> {
    TOOLS.iter().map(|t| ToolInfo {
        name: t.name,
        description: match t.remote {
            Remote::Only(_) => format!("{} {ELSEWHERE}", t.about),
            Remote::No | Remote::Also => t.about.into(),
        },
        // Its switch decides for another member too (ADR 0026).
        brain: t.reach.offers(Surface::Run) || matches!(t.remote, Remote::Only(_)),
        mcp: t.reach.offers(Surface::Mcp),
        off_by_default: t.off_by_default,
    })
}

/// A tool call's arguments, checked against the tool's parameters.
struct Args {
    tool: &'static str,
    values: Map<String, Value>,
}

impl Args {
    /// Refuses names the tool doesn't take, values of the wrong type and
    /// missing required arguments. A null value counts as absent.
    fn parse(tool: &Tool, args: Value) -> Result<Self, String> {
        let given = match args {
            Value::Object(given) => given,
            Value::Null => Map::new(),
            _ => {
                return Err(format!(
                    "The arguments for {} must be a JSON object.",
                    tool.name
                ));
            }
        };
        let mut values = Map::new();
        for (name, value) in given {
            let Some(param) = tool.params.iter().find(|p| p.name == name) else {
                return Err(format!("{} has no argument {name}.", tool.name));
            };
            if value.is_null() {
                continue;
            }
            if !param.kind.accepts(&value) {
                return Err(format!("{name} must be {}.", param.kind.noun()));
            }
            values.insert(name, value);
        }
        if let Some(missing) = tool
            .params
            .iter()
            .find(|p| p.required && !values.contains_key(p.name))
        {
            return Err(format!("{} needs {}.", tool.name, missing.name));
        }
        Ok(Self {
            tool: tool.name,
            values,
        })
    }

    fn int(&self, name: &str) -> Option<i64> {
        whole(self.values.get(name)?)
    }

    fn text(&self, name: &str) -> Option<&str> {
        self.values.get(name)?.as_str()
    }

    fn flag(&self, name: &str) -> Option<bool> {
        self.values.get(name)?.as_bool()
    }

    fn need_int(&self, name: &str) -> Result<i64, String> {
        self.int(name).ok_or_else(|| self.missing(name))
    }

    fn need_text(&self, name: &str) -> Result<&str, String> {
        self.text(name).ok_or_else(|| self.missing(name))
    }

    fn missing(&self, name: &str) -> String {
        format!("{} needs {name}.", self.tool)
    }
}

/// The tools on one surface for one caller, over the Brain's command path.
pub struct DeviceTools {
    inputs: mpsc::Sender<Input>,
    status: watch::Receiver<Snapshot>,
    live: Arc<AssistLive>,
    surface: Surface,
    caller: Caller,
    kit: ToolKit,
    /// How long a call may take; none without it.
    within: Option<Duration>,
}

/// What every [`DeviceTools`] shares, built once: the network tools' HTTP
/// clients, so connections are reused across requests, the counters, and
/// the satellite group.
#[derive(Clone)]
pub struct ToolKit {
    weather: OpenMeteo,
    radio: RadioBrowser,
    counters: Counters,
    /// What the update tools use; without one they answer with an error.
    updater: Option<Arc<Updater>>,
    group: Option<Arc<Group>>,
}

impl ToolKit {
    pub fn new(counters: Counters) -> Self {
        Self {
            weather: OpenMeteo::default(),
            radio: RadioBrowser::default(),
            counters,
            updater: None,
            group: None,
        }
    }

    /// The updater the update tools check and install with.
    pub fn with_updater(mut self, updater: Arc<Updater>) -> Self {
        self.updater = Some(updater);
        self
    }

    /// The satellite group (ADR 0026), for `send_message` and the request's
    /// context. Without it, the brain gets neither.
    pub fn with_group(mut self, group: Arc<Group>) -> Self {
        self.group = Some(group);
        self
    }

    pub fn group(&self) -> Option<&Arc<Group>> {
        self.group.as_ref()
    }

    /// The weather from another address, for tests.
    #[cfg(test)]
    fn with_weather(mut self, weather: OpenMeteo) -> Self {
        self.weather = weather;
        self
    }
}

impl DeviceTools {
    pub fn new(
        kit: &ToolKit,
        inputs: mpsc::Sender<Input>,
        status: watch::Receiver<Snapshot>,
        live: Arc<AssistLive>,
        surface: Surface,
        caller: Caller,
    ) -> Self {
        Self {
            inputs,
            status,
            live,
            surface,
            caller,
            kit: kit.clone(),
            within: None,
        }
    }

    /// End each call after `limit`. Whatever reached the Brain by then may
    /// still take effect.
    pub fn within(mut self, limit: Duration) -> Self {
        self.within = Some(limit);
        self
    }

    /// The owner's switch for a tool; a tool without one is on, unless it
    /// is off by default. An owner's MCP tool is not in the table: on.
    fn switched_on(&self, name: &str) -> bool {
        self.live
            .switches
            .get(name)
            .copied()
            .unwrap_or_else(|| !TOOLS.iter().any(|t| t.name == name && t.off_by_default))
    }

    /// Whether these are a brain's tools, and its satellite is in a group
    /// with another member in standalone mode (ADR 0026): then the brain
    /// gets `send_message`, and acts on the other members.
    fn grouped(&self) -> bool {
        self.surface == Surface::Run && self.kit.group().is_some_and(|g| message::offered(g))
    }

    /// Whether this surface offers `tool` now, `grouped` as
    /// [`Self::grouped`] says.
    fn offers(&self, tool: &Tool, grouped: bool) -> bool {
        match (self.surface, tool.remote) {
            (Surface::Run, Remote::Only(_)) => grouped,
            _ => tool.reach.offers(self.surface) && (tool.name != message::NAME || grouped),
        }
    }

    /// Sends a command to the Brain and waits for its answer. A run's
    /// command is fenced by its id; an MCP call never waits for queue space
    /// and leaves room for the wake word.
    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, String>>) -> ApiCommand,
    ) -> Result<T, String> {
        let (reply, answer) = oneshot::channel();
        let cmd = make(reply);
        match self.caller {
            Caller::Run(run) => self
                .inputs
                .send(Input::RunTool { run, cmd })
                .await
                .map_err(|_| SHUTTING_DOWN.to_owned())?,
            Caller::Mcp | Caller::Peer => {
                if self.inputs.capacity() < BRAIN_RESERVE {
                    return Err(BUSY.into());
                }
                self.inputs.try_send(Input::Api(cmd)).map_err(|e| match e {
                    TrySendError::Full(_) => BUSY.to_owned(),
                    TrySendError::Closed(_) => SHUTTING_DOWN.to_owned(),
                })?;
            }
        }
        answer.await.unwrap_or_else(|_| Err(SHUTTING_DOWN.into()))
    }

    /// The owner's MCP tools on offer, each with its server; only `name`'s
    /// when given. Only a tool-loop brain gets them. The first server to
    /// list a name has it; a server that lists nothing is left out.
    async fn owner_tools(&self, name: Option<&str>) -> Vec<(&OwnerMcp, ToolSpec)> {
        if self.surface != Surface::Run || self.live.behaviour().delegation != Delegation::ToolLoop
        {
            return Vec::new();
        }
        // Only a server whose namespace the name is in can have it.
        let servers =
            self.live.mcp.iter().filter(|server| {
                name.is_none_or(|name| pinna_mcp::in_namespace(&server.name, name))
            });
        let listed = join_all(
            servers.map(|server| async move { (server.as_ref(), server.tools.list().await) }),
        )
        .await;
        let mut seen = HashSet::new();
        let mut offered = Vec::new();
        for (server, listing) in listed {
            let specs = match listing {
                Ok(specs) if !specs.is_empty() => specs,
                Ok(_) => {
                    if server.left_out(Instant::now()) {
                        tracing::warn!(
                            "the MCP server {} listed no tools the brain can use; it goes without it",
                            server.name
                        );
                    }
                    continue;
                }
                Err(error) => {
                    if server.left_out(Instant::now()) {
                        tracing::warn!(
                            "the MCP server {} can't be used: {error} The brain goes without it.",
                            server.name
                        );
                    }
                    continue;
                }
            };
            for spec in specs {
                if name.is_none_or(|name| spec.name == name)
                    && device_tool_name(&spec.name).is_none()
                    && seen.insert(spec.name.clone())
                {
                    offered.push((server, spec));
                }
            }
        }
        offered
    }
}

impl ToolSet for DeviceTools {
    fn specs(&self) -> BoxFuture<'_, Vec<ToolSpec>> {
        Box::pin(async move {
            let grouped = self.grouped();
            let device = TOOLS
                .iter()
                .filter(|t| self.offers(t, grouped))
                .map(|t| if grouped { t.grouped_spec() } else { t.spec() });
            let owner = self.owner_tools(None).await.into_iter().map(|(_, s)| s);
            device
                .chain(owner)
                .filter(|spec| self.switched_on(&spec.name))
                .collect()
        })
    }

    fn call<'a>(&'a self, name: &'a str, args: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let started = Instant::now();
            let dispatched = self.dispatch(name, args);
            let (output, server) = match self.within {
                None => dispatched.await,
                Some(limit) => tokio::time::timeout(limit, dispatched)
                    .await
                    .unwrap_or_else(|_| (ToolOutput::error(too_slow(name, limit)), None)),
            };
            let via = match (self.caller, self.surface) {
                (Caller::Peer, _) => Via::Peer,
                (_, Surface::Run) => Via::Run,
                (_, Surface::Mcp) => Via::Mcp,
            };
            self.kit
                .counters
                .tool_call(name, server, via, !output.is_error);
            log_call(self.caller, name, server, &output, started.elapsed());
            let run = match self.caller {
                Caller::Run(run) => run.0,
                _ => 0,
            };
            crate::timeline::stage(
                || {
                    // A name the model made up is not repeated, as in the log.
                    if pinna_mcp::tool_name(name) {
                        format!("tool {name}")
                    } else {
                        "tool with an invalid name".to_owned()
                    }
                },
                started.elapsed(),
                run,
            );
            output
        })
    }
}

/// Why a call with a time limit ended without its result.
fn too_slow(name: &str, limit: Duration) -> String {
    let tool = device_tool_name(name).unwrap_or("The tool");
    format!(
        "{tool} didn't finish within {} seconds. It may still take effect.",
        limit.as_secs_f64()
    )
}

/// One line per tool call (ADR 0017): who called, which tool, whether it
/// worked and how long it took. Never the arguments or the output, which can
/// hold what was said. A name the model made up is not repeated either.
fn log_call(caller: Caller, name: &str, server: Option<&str>, output: &ToolOutput, took: Duration) {
    let who = match caller {
        Caller::Run(run) => format!("request {}", run.0),
        Caller::Mcp => "an MCP client".to_owned(),
        Caller::Peer => "another satellite of the group".to_owned(),
    };
    let name = if pinna_mcp::tool_name(name) {
        name
    } else {
        "with an invalid name"
    };
    let from = server
        .map(|s| format!(" (MCP server {s})"))
        .unwrap_or_default();
    let outcome = if output.is_error { "error" } else { "ok" };
    tracing::info!(
        "{who}: tool {name}{from}: {outcome}, {} ms",
        took.as_millis()
    );
}

impl DeviceTools {
    /// Where the brain runs `tool`, and its arguments without `satellite`:
    /// on another member when `satellite` names one (ADR 0026). Only the
    /// brain names one; `/mcp` and the group refuse `satellite` as an
    /// argument the tool doesn't take. The run's tool list may have offered
    /// `satellite` while another member was in standalone mode, so it is
    /// taken out even once none is: no name, or this satellite's, runs
    /// here, and another member's name hears why that member is out of
    /// reach.
    fn target(&self, tool: &Tool, args: Value) -> Result<(Value, Target), String> {
        let group = match self.kit.group() {
            Some(group) if self.surface == Surface::Run && tool.remote != Remote::No => group,
            _ => return Ok((args, Target::Here)),
        };
        let mut given = match args {
            Value::Object(given) => given,
            Value::Null => Map::new(),
            _ => {
                return Err(format!(
                    "The arguments for {} must be a JSON object.",
                    tool.name
                ));
            }
        };
        let satellite = match given.remove(SATELLITE) {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) => Some(name),
            Some(_) => return Err(format!("{SATELLITE} must be text.")),
        };
        let (own, host) = {
            let status = self.status.borrow();
            (status.friendly_name.clone(), status.name.clone())
        };
        let target = remote::target(group, &own, &host, tool.name, satellite.as_deref())?;
        if matches!(tool.remote, Remote::Only(_)) && matches!(target, Target::Here) {
            // In no group now: there is no satellite for the brain to name.
            if group.other_members().is_empty() {
                return Err(remote::NO_GROUP.into());
            }
            return Err(match satellite.as_deref().map(str::trim) {
                None | Some("") => format!("{} needs {SATELLITE}.", tool.name),
                Some(_) => remote::not_here(group, tool.name),
            });
        }
        Ok((Value::Object(given), target))
    }

    /// Run a device tool, or an owner MCP server's for a tool-loop brain.
    /// Returns the output and, for an MCP server's tool, the server that
    /// answered.
    async fn dispatch(&self, name: &str, args: Value) -> (ToolOutput, Option<&str>) {
        let switched_off =
            || ToolOutput::error(format!("The owner switched {name} off under Assistant."));
        let grouped = self.grouped();
        let Some(tool) = TOOLS.iter().find(|t| {
            t.name == name
                && (t.reach.offers(self.surface)
                    // The run's tool list may have offered it for another
                    // member: target() says why that one is out of reach.
                    || (matches!(t.remote, Remote::Only(_))
                        && self.surface == Surface::Run
                        && self.kit.group().is_some()))
        }) else {
            let Some((server, _)) = self.owner_tools(Some(name)).await.pop() else {
                return (ToolOutput::error(format!("Unknown tool {name}.")), None);
            };
            if !self.switched_on(name) {
                return (switched_off(), Some(&server.name));
            }
            return (server.tools.call(name, args).await, Some(&server.name));
        };
        if !self.switched_on(name) {
            // Never turned on: the owner didn't switch it off.
            if tool.off_by_default && !self.live.switches.contains_key(name) {
                return (
                    ToolOutput::error(format!(
                        "{name} is off until the owner turns it on under Assistant, Tools."
                    )),
                    None,
                );
            }
            return (switched_off(), None);
        }
        // Offered when the run's tool list was made, but not now. A tool
        // that acts on another member says instead why that one is out of
        // reach.
        if tool.remote == Remote::No && !self.offers(tool, grouped) {
            return (ToolOutput::error(message::NO_ONE), None);
        }
        let result = match self.target(tool, args) {
            Err(why) => Err(why),
            Ok((args, Target::Here)) => match Args::parse(tool, args) {
                Ok(args) => device::run(self, tool.op, &args).await,
                Err(why) => Err(why),
            },
            // Checked here too, so a wrong argument costs no call.
            Ok((args, Target::Member(member, mode))) => match Args::parse(tool, args.clone()) {
                Ok(_) => match self.kit.group() {
                    Some(group) => remote::call(group, &member, mode, tool.name, args).await,
                    None => Err(remote::NO_GROUP.into()),
                },
                Err(why) => Err(why),
            },
        };
        let output = match result {
            Ok(text) => ToolOutput::ok(text),
            Err(why) => ToolOutput::error(why),
        };
        (output, None)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use pinna_assist::config::{AssistConfig, Connection, SlotConfig};
    use pinna_assist::pipeline::Pipeline;
    use pinna_assist::presets::PresetId;
    use pinna_assist::secrets::Secrets;
    use pinna_esphome::{DeviceEvent, EsphomeHandle};
    use pinna_http::Secret;

    use super::*;
    use crate::leds::LedState;
    use crate::settings::{Mode, Settings};
    use crate::updates::UpdateChannel;
    use crate::voice::{Brain, Defaults, Options};

    /// A real Brain on its own task, as the dashboard reaches it.
    struct Device {
        inputs: mpsc::Sender<Input>,
        status: watch::Receiver<Snapshot>,
        events: mpsc::Receiver<DeviceEvent>,
        _dir: tempfile::TempDir,
    }

    impl Device {
        async fn start(mode: Mode) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (esp, events) = EsphomeHandle::detached(256);
            let (status_tx, mut status) = watch::channel(Snapshot::default());
            let (inputs, rx) = mpsc::channel(64);
            let brain = Brain::new(
                esp,
                None,
                Arc::default(),
                watch::channel(LedState::default()).0,
                status_tx,
                inputs.clone(),
                Options {
                    key_file: dir.path().join("key"),
                    settings_file: dir.path().join("settings.toml"),
                    wake_words: crate::wakewords::test_catalogue(),
                    active_wake_words: vec!["alexa_v0.1".into()],
                    keywords: None,
                    defaults: Defaults {
                        node_name: "pinna-test".into(),
                        friendly_name: "Test".into(),
                        default_name: true,
                        wake_sound: false,
                        update_channel: crate::updates::UpdateChannel::Off,
                    },
                    updates: None,
                    mdns: None,
                    encrypted: false,
                    has_hw_privacy: false,
                    privacy_trigger: Vec::new(),
                    mute_lamp: None,
                    airplay: false,
                    bluetooth: false,
                    sendspin: None,
                    settings: Settings {
                        mode,
                        ..Settings::default()
                    },
                },
            );
            tokio::spawn(brain.run(rx));
            // The Brain's first snapshot names the device.
            status
                .wait_for(|s| s.name == "pinna-test")
                .await
                .expect("the Brain published no snapshot");
            Self {
                inputs,
                status,
                events,
                _dir: dir,
            }
        }

        /// As an MCP client, so the Brain applies the commands whatever runs.
        fn tools(&self, surface: Surface, switches: &[(&str, bool)]) -> DeviceTools {
            DeviceTools::new(
                &crate::tools::ToolKit::new(crate::metrics::Counters::default()),
                self.inputs.clone(),
                self.status.clone(),
                live(switches),
                surface,
                Caller::Mcp,
            )
        }

        fn events(&mut self) -> Vec<DeviceEvent> {
            let mut events = Vec::new();
            while let Ok(event) = self.events.try_recv() {
                events.push(event);
            }
            events
        }
    }

    fn live(switches: &[(&str, bool)]) -> Arc<AssistLive> {
        let mut live = AssistLive::empty();
        live.switches = switches
            .iter()
            .map(|&(name, on)| (name.to_owned(), on))
            .collect::<BTreeMap<_, _>>();
        Arc::new(live)
    }

    /// Tools over a bare channel, to see exactly what reaches the Brain.
    fn bare(surface: Surface, caller: Caller) -> (DeviceTools, mpsc::Receiver<Input>) {
        let (inputs, rx) = mpsc::channel(64);
        let status = watch::channel(Snapshot::default()).1;
        let tools = DeviceTools::new(
            &crate::tools::ToolKit::new(crate::metrics::Counters::default()),
            inputs,
            status,
            live(&[]),
            surface,
            caller,
        );
        (tools, rx)
    }

    async fn names(tools: &DeviceTools) -> Vec<String> {
        tools.specs().await.into_iter().map(|s| s.name).collect()
    }

    #[tokio::test]
    async fn clearing_no_notification_says_so_and_points_at_the_light() {
        let (tools, mut rx) = bare(Surface::Mcp, Caller::Mcp);
        let out = tools.call("clear_notification", json!({})).await;
        assert!(!out.is_error, "{}", out.text);
        assert!(
            out.text.starts_with("No notification is showing"),
            "{}",
            out.text
        );
        assert!(out.text.contains("ring_light"), "{}", out.text);
        assert!(rx.try_recv().is_err(), "nothing reaches the Brain");
    }

    #[tokio::test]
    async fn set_volume_takes_the_dashboard_path_to_the_brain() {
        let mut d = Device::start(Mode::Standalone).await;
        let tools = d.tools(Surface::Mcp, &[]);

        let out = tools.call("set_volume", json!({"percent": 40})).await;

        assert_eq!(out, ToolOutput::ok("The volume is 40%."));
        assert_eq!(d.status.borrow().volume, 40);
        assert!(d.events().contains(&DeviceEvent::NumberState {
            object_id: "volume".into(),
            value: 40.0,
        }));
    }

    #[tokio::test]
    async fn change_volume_stays_between_silent_and_full() {
        let d = Device::start(Mode::Standalone).await;
        let tools = d.tools(Surface::Mcp, &[]);

        let up = tools.call("change_volume", json!({"by": 10})).await;
        assert_eq!(up, ToolOutput::ok("The volume is 60%."));
        let down = tools.call("change_volume", json!({"by": -500})).await;
        assert_eq!(down, ToolOutput::ok("The volume is 0%."));
        assert_eq!(d.status.borrow().volume, 0);
    }

    #[tokio::test]
    async fn no_tool_unmutes_the_microphone() {
        let mut d = Device::start(Mode::Standalone).await;
        for surface in [Surface::Run, Surface::Mcp] {
            let tools = d.tools(surface, &[]);
            assert!(!names(&tools).await.iter().any(|n| n.contains("unmute")));
            let mute = tools
                .specs()
                .await
                .into_iter()
                .find(|s| s.name == "mute_microphone")
                .unwrap();
            assert_eq!(mute.input_schema["properties"], json!({}));
        }
        let tools = d.tools(Surface::Mcp, &[]);
        let unmute = tools.call("unmute_microphone", json!({})).await;
        assert_eq!(unmute, ToolOutput::error("Unknown tool unmute_microphone."));
        let off = tools.call("mute_microphone", json!({"on": false})).await;
        assert_eq!(
            off,
            ToolOutput::error("mute_microphone has no argument on.")
        );
        assert!(!d.status.borrow().sw_muted);

        let on = tools.call("mute_microphone", json!({})).await;

        assert_eq!(
            on,
            ToolOutput::ok("The microphone is muted. Only a person can unmute it.")
        );
        assert!(d.status.borrow().sw_muted);
        assert!(d.events().contains(&DeviceEvent::MuteState(true)));
    }

    #[tokio::test]
    async fn both_surfaces_offer_switch_assistant() {
        let info = catalogue().find(|t| t.name == "switch_assistant").unwrap();
        assert!(info.brain && info.mcp);
        let (run, _rx) = bare(Surface::Run, Caller::Run(RunId(1)));
        let (mcp, _rx) = bare(Surface::Mcp, Caller::Mcp);
        for tools in [&run, &mcp] {
            let spec = tools
                .specs()
                .await
                .into_iter()
                .find(|s| s.name == "switch_assistant")
                .unwrap();
            assert_eq!(
                spec.input_schema["properties"]["mode"]["enum"],
                json!(["home_assistant", "standalone"])
            );
            assert_eq!(spec.input_schema["required"], json!(["mode"]));
        }
    }

    #[tokio::test]
    async fn switch_assistant_says_what_happens() {
        let d = Device::start(Mode::Standalone).await;
        let tools = d.tools(Surface::Mcp, &[]);

        let same = tools
            .call("switch_assistant", json!({"mode": "standalone"}))
            .await;
        assert_eq!(
            same,
            ToolOutput::ok("The satellite is already in Standalone mode.")
        );
        let other = tools
            .call("switch_assistant", json!({"mode": "bluetooth"}))
            .await;
        assert!(other.is_error);
    }

    #[tokio::test]
    async fn a_switched_off_switch_assistant_is_refused() {
        let d = Device::start(Mode::Standalone).await;
        let tools = d.tools(Surface::Mcp, &[("switch_assistant", false)]);

        let out = tools
            .call("switch_assistant", json!({"mode": "home_assistant"}))
            .await;

        assert_eq!(
            out,
            ToolOutput::error("The owner switched switch_assistant off under Assistant.")
        );
        assert_eq!(d.status.borrow().mode, Mode::Standalone);
    }

    #[tokio::test]
    async fn a_non_http_address_is_refused_before_the_brain() {
        let (tools, mut rx) = bare(Surface::Run, Caller::Run(RunId(1)));

        let out = tools
            .call("play_url", json!({"url": "ftp://radio.test/live"}))
            .await;

        assert!(out.is_error);
        assert_eq!(
            out.text,
            "the URL must start with http:// or https:// and be at most 2048 characters"
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_switched_off_tool_is_absent_and_refused() {
        let d = Device::start(Mode::Standalone).await;
        for surface in [Surface::Run, Surface::Mcp] {
            let tools = d.tools(surface, &[("set_volume", false), ("stop_media", true)]);
            let names = names(&tools).await;
            assert!(!names.contains(&"set_volume".to_owned()));
            assert!(names.contains(&"stop_media".to_owned()));
            assert!(names.contains(&"change_volume".to_owned()));
        }
        let tools = d.tools(Surface::Mcp, &[("set_volume", false)]);

        let out = tools.call("set_volume", json!({"percent": 90})).await;

        assert_eq!(
            out,
            ToolOutput::error("The owner switched set_volume off under Assistant.")
        );
        assert_eq!(d.status.borrow().volume, 50);
    }

    #[tokio::test]
    async fn each_surface_offers_only_its_tools() {
        let (run, _rx) = bare(Surface::Run, Caller::Run(RunId(1)));
        let (mcp, _rx) = bare(Surface::Mcp, Caller::Mcp);
        let run_names = names(&run).await;
        let mcp_names = names(&mcp).await;
        for name in ["speak", "cancel_request", "status"] {
            assert!(!run_names.iter().any(|n| n == name), "{name}");
        }
        assert!(!mcp_names.iter().any(|n| n == "weather"));
        for name in [
            "cancel_request",
            "status",
            "set_timer",
            "mute_microphone",
            "speak",
        ] {
            assert!(mcp_names.iter().any(|n| n == name), "{name}");
        }
        let out = run.call("cancel_request", json!({})).await;
        assert_eq!(out, ToolOutput::error("Unknown tool cancel_request."));
        assert!(!Reach::RunOnly.offers(Surface::Mcp));
        assert!(!Reach::McpOnly.offers(Surface::Run));
        assert!(Reach::Both.offers(Surface::Run) && Reach::Both.offers(Surface::Mcp));
    }

    #[tokio::test]
    async fn the_catalogue_lists_each_tool_with_its_surfaces() {
        let (run, _rx) = bare(Surface::Run, Caller::Run(RunId(1)));
        let (mcp, _rx) = bare(Surface::Mcp, Caller::Mcp);
        let run_names = names(&run).await;
        let mcp_names = names(&mcp).await;

        let listed: Vec<ToolInfo> = catalogue().collect();

        assert_eq!(listed.len(), TOOLS.len());
        for tool in &listed {
            // A tool that starts off is not in a list until it is turned on.
            // send_message is the brain's, but offered only in a group with
            // another member in standalone mode; cancel_request is the
            // brain's there too, for another member only.
            let offered = run_names.iter().any(|n| n == tool.name)
                || tool.off_by_default
                || [message::NAME, "cancel_request"].contains(&tool.name);
            assert_eq!(tool.brain, offered, "{}", tool.name);
            assert_eq!(tool.mcp, mcp_names.iter().any(|n| n == tool.name));
            assert!(tool.brain || tool.mcp, "{}", tool.name);
            assert!(!tool.description.is_empty(), "{}", tool.name);
        }
        let weather = listed.iter().find(|t| t.name == "weather").unwrap();
        assert!(weather.brain && !weather.mcp);
        let speak = listed.iter().find(|t| t.name == "speak").unwrap();
        assert!(!speak.brain && speak.mcp);
        let cancel = listed.iter().find(|t| t.name == "cancel_request").unwrap();
        assert!(cancel.brain && cancel.mcp);
        // The owner reads what /mcp does, and that the brain does it only
        // elsewhere (ADR 0017, ADR 0026).
        assert_eq!(
            cancel.description,
            "Stop the request the satellite is handling, or silence a ringing alarm. The brain \
             does this only on another satellite of its group."
        );
        assert_eq!(
            speak.description,
            TOOLS.iter().find(|t| t.name == "speak").unwrap().about
        );
    }

    #[tokio::test]
    async fn a_run_sends_its_commands_fenced_by_its_id() {
        let (tools, mut rx) = bare(Surface::Run, Caller::Run(RunId(7)));
        let call =
            tokio::spawn(async move { tools.call("set_volume", json!({"percent": 30})).await });

        match rx.recv().await.unwrap() {
            Input::RunTool {
                run,
                cmd: ApiCommand::SetVolume(30, reply),
            } => {
                assert_eq!(run, RunId(7));
                let s = Snapshot {
                    volume: 30,
                    ..Default::default()
                };
                reply.send(Ok(s)).unwrap();
            }
            other => panic!("unexpected input {other:?}"),
        }
        assert_eq!(call.await.unwrap(), ToolOutput::ok("The volume is 30%."));
    }

    #[tokio::test]
    async fn a_run_hears_when_its_mute_takes_effect() {
        let (tools, mut rx) = bare(Surface::Run, Caller::Run(RunId(2)));
        let call = tokio::spawn(async move { tools.call("mute_microphone", json!({})).await });

        match rx.recv().await.unwrap() {
            Input::RunTool {
                run: RunId(2),
                cmd: ApiCommand::VoiceMute(reply),
            } => reply.send(Ok(Snapshot::default())).unwrap(),
            other => panic!("unexpected input {other:?}"),
        }
        assert_eq!(
            call.await.unwrap(),
            ToolOutput::ok(
                "The microphone mutes when this reply ends. Only a person can unmute it."
            )
        );
    }

    #[tokio::test]
    async fn speak_checks_its_text_before_the_brain() {
        let (tools, mut rx) = bare(Surface::Mcp, Caller::Mcp);

        let long = tools.call("speak", json!({"text": "a".repeat(501)})).await;

        assert_eq!(
            long,
            ToolOutput::error("The text to say is at most 500 characters.")
        );
        assert!(rx.try_recv().is_err());

        let call =
            tokio::spawn(async move { tools.call("speak", json!({"text": " Hello. "})).await });
        match rx.recv().await.unwrap() {
            Input::Api(ApiCommand::Speak(text, reply)) => {
                assert_eq!(text, "Hello.");
                reply.send(Ok(())).unwrap();
            }
            other => panic!("unexpected input {other:?}"),
        }
        assert_eq!(call.await.unwrap(), ToolOutput::ok("Speaking."));
    }

    #[tokio::test]
    async fn speak_needs_a_speaker() {
        let d = Device::start(Mode::Standalone).await;
        let tools = d.tools(Surface::Mcp, &[]);

        let out = tools.call("speak", json!({"text": "Hello."})).await;

        assert!(out.is_error);
        assert!(
            out.text
                .starts_with("This satellite has no working speaker")
        );
    }

    #[tokio::test]
    async fn mcp_leaves_the_last_queue_slots_to_the_device() {
        let (inputs, _rx) = mpsc::channel(64);
        while inputs.capacity() >= BRAIN_RESERVE {
            inputs.try_send(Input::Privacy(false)).unwrap();
        }
        let status = watch::channel(Snapshot::default()).1;
        let tools = DeviceTools::new(
            &crate::tools::ToolKit::new(crate::metrics::Counters::default()),
            inputs.clone(),
            status,
            live(&[]),
            Surface::Mcp,
            Caller::Mcp,
        );

        let out = tools.call("set_volume", json!({"percent": 30})).await;

        assert_eq!(out, ToolOutput::error(BUSY));
        assert_eq!(inputs.capacity(), BRAIN_RESERVE - 1);
    }

    #[tokio::test]
    async fn a_stopped_brain_is_reported() {
        let (tools, rx) = bare(Surface::Mcp, Caller::Mcp);
        drop(rx);
        let out = tools.call("stop_media", json!({})).await;
        assert_eq!(out, ToolOutput::error(SHUTTING_DOWN));
    }

    #[tokio::test]
    async fn timers_are_set_listed_and_cancelled_by_name() {
        let d = Device::start(Mode::Standalone).await;
        let tools = d.tools(Surface::Mcp, &[]);

        let set = tools
            .call("set_timer", json!({"seconds": 300, "name": "Tea"}))
            .await;
        assert!(!set.is_error, "{}", set.text);
        let id = d.status.borrow().timers[0].id.clone();
        assert_eq!(
            set.text,
            format!("Timer \"Tea\" set for 5 minutes (id {id}).")
        );
        let listed: Value =
            serde_json::from_str(&tools.call("list_timers", json!({})).await.text).unwrap();
        assert_eq!(listed["timers"][0]["id"], id.as_str());
        assert_eq!(listed["timers"][0]["name"], "Tea");
        assert_eq!(listed["timers"][0]["total_seconds"], 300);

        let cancelled = tools.call("cancel_timer", json!({"timer": "Tea"})).await;

        assert_eq!(cancelled, ToolOutput::ok("The timer is cancelled."));
        assert!(d.status.borrow().timers.is_empty());
        let listed = tools.call("list_timers", json!({})).await;
        assert_eq!(listed, ToolOutput::ok(r#"{"timers":[]}"#));
    }

    #[tokio::test]
    async fn the_brain_refusal_comes_back_as_an_error() {
        let d = Device::start(Mode::HomeAssistant).await;
        let tools = d.tools(Surface::Mcp, &[]);

        let out = tools.call("set_timer", json!({"seconds": 60})).await;

        assert_eq!(
            out,
            ToolOutput::error("Device timers are available in standalone mode only.")
        );
    }

    #[tokio::test]
    async fn ring_light_and_reply_volume_reach_the_brain() {
        let mut d = Device::start(Mode::Standalone).await;
        let tools = d.tools(Surface::Mcp, &[]);

        let light = tools
            .call(
                "ring_light",
                json!({"on": true, "brightness": 50, "color": "#ff8000", "effect": "Pulse"}),
            )
            .await;
        assert_eq!(
            light,
            ToolOutput::ok("The ring light is on at 50%, colour #ff8000, effect Pulse.")
        );
        assert!(d.status.borrow().ring_light.on);
        assert!(
            d.events()
                .iter()
                .any(|e| matches!(e, DeviceEvent::LightState { on: true, .. }))
        );
        let nothing = tools.call("ring_light", json!({})).await;
        assert!(nothing.is_error);

        let reply = tools.call("set_reply_volume", json!({"percent": 20})).await;

        assert_eq!(reply, ToolOutput::ok("Replies play at 20%."));
        assert_eq!(d.status.borrow().settings.reply_volume, Some(20));
    }

    #[tokio::test]
    async fn status_reads_the_snapshot_without_the_brain() {
        let d = Device::start(Mode::Standalone).await;
        let tools = d.tools(Surface::Mcp, &[]);

        let out = tools.call("status", json!({})).await;

        assert!(!out.is_error);
        let shown: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(shown["mode"], "standalone");
        assert_eq!(shown["phase"], "idle");
        assert_eq!(shown["volume"], 50);
    }

    #[tokio::test]
    async fn arguments_are_checked_against_the_table() {
        let (tools, mut rx) = bare(Surface::Mcp, Caller::Mcp);
        let cases = [
            (
                "set_volume",
                json!([40]),
                "The arguments for set_volume must be a JSON object.",
            ),
            ("set_volume", json!({}), "set_volume needs percent."),
            (
                "set_volume",
                json!({"percent": null}),
                "set_volume needs percent.",
            ),
            (
                "set_volume",
                json!({"percent": "40"}),
                "percent must be a whole number.",
            ),
            (
                "set_volume",
                json!({"percent": 40.5}),
                "percent must be a whole number.",
            ),
            (
                "set_volume",
                json!({"percent": 101}),
                "percent must be between 0 and 100",
            ),
            (
                "set_volume",
                json!({"percent": -1}),
                "percent must be between 0 and 100",
            ),
            (
                "set_volume",
                json!({"percent": 40, "loud": true}),
                "set_volume has no argument loud.",
            ),
            (
                "ring_light",
                json!({"effect": 2}),
                "effect must be one of Solid, Pulse, Flash.",
            ),
            (
                "ring_light",
                json!({"effect": "Rainbow"}),
                "effect must be one of Solid, Pulse, Flash",
            ),
            (
                "ring_light",
                json!({"on": "yes"}),
                "on must be true or false.",
            ),
            ("cancel_timer", json!({}), "cancel_timer needs timer."),
        ];
        for (name, args, why) in cases {
            // Nothing answers on the bare channel: a call that got through would wait.
            let out = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                tools.call(name, args.clone()),
            )
            .await
            .unwrap_or_else(|_| panic!("{name} {args} reached the Brain"));
            assert_eq!(out, ToolOutput::error(why), "{name} {args}");
        }
        assert!(rx.try_recv().is_err());

        let (tools, mut rx) = bare(Surface::Mcp, Caller::Mcp);
        let call =
            tokio::spawn(async move { tools.call("set_volume", json!({"percent": 40.0})).await });
        match rx.recv().await.unwrap() {
            Input::Api(ApiCommand::SetVolume(40, reply)) => {
                reply.send(Err("refused".into())).unwrap()
            }
            other => panic!("unexpected input {other:?}"),
        }
        assert_eq!(call.await.unwrap(), ToolOutput::error("refused"));
    }

    #[tokio::test]
    async fn every_spec_is_a_closed_object_schema() {
        let (tools, _rx) = bare(Surface::Mcp, Caller::Mcp);
        let mut specs = tools.specs().await;
        // The tool that starts off, turned on.
        let run = with_updater(Surface::Run, None, &[("install_update", true)]);
        specs.extend(run.specs().await);
        // Offered only in a group, where the brain's take `satellite`.
        let send = TOOLS.iter().find(|t| t.name == message::NAME).unwrap();
        specs.push(send.spec());
        specs.extend(TOOLS.iter().map(Tool::grouped_spec));
        let mut seen = HashSet::new();
        for spec in &specs {
            let schema = &spec.input_schema;
            assert!(pinna_mcp::namespaced("pinna", &spec.name).is_some());
            assert_eq!(schema["type"], "object");
            assert_eq!(schema["additionalProperties"], false);
            for required in schema["required"].as_array().unwrap() {
                let name = required.as_str().unwrap();
                assert!(schema["properties"].get(name).is_some(), "{name}");
            }
            seen.insert(spec.name.clone());
        }
        assert_eq!(seen.len(), TOOLS.len());
    }

    /// Tools on one of the owner's MCP servers; each says it ran.
    struct Remote(&'static [&'static str]);

    impl ToolSet for Remote {
        fn specs(&self) -> BoxFuture<'_, Vec<ToolSpec>> {
            let specs = self
                .0
                .iter()
                .map(|name| ToolSpec {
                    name: (*name).into(),
                    description: format!("Remote {name}."),
                    input_schema: json!({"type": "object"}),
                })
                .collect();
            Box::pin(std::future::ready(specs))
        }

        fn call<'a>(&'a self, name: &'a str, _args: Value) -> BoxFuture<'a, ToolOutput> {
            Box::pin(std::future::ready(ToolOutput::ok(format!(
                "remote {name} ran"
            ))))
        }
    }

    /// An owner's MCP server on 127.0.0.1 offering `tools`, with a token in
    /// its address.
    async fn remote(tools: &'static [&'static str]) -> String {
        use axum::response::IntoResponse;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp/token", listener.local_addr().unwrap());
        let tools = Arc::new(Remote(tools));
        let answer = move |body: axum::body::Bytes| {
            let tools = tools.clone();
            async move {
                let info = pinna_mcp::server::ServerInfo {
                    name: "remote".into(),
                    version: "1".into(),
                };
                match pinna_mcp::server::handle(tools.as_ref(), &info, &body)
                    .await
                    .reply
                {
                    Some(reply) => (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        reply,
                    )
                        .into_response(),
                    None => axum::http::StatusCode::ACCEPTED.into_response(),
                }
            }
        };
        let app = axum::Router::new().route("/mcp/token", axum::routing::post(answer));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// An address nothing answers on.
    async fn down() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}/mcp", listener.local_addr().unwrap())
    }

    /// A pipeline whose brain runs the tool loop or hands requests off.
    fn pipeline(delegation: Delegation) -> Arc<Pipeline> {
        let mut cfg = AssistConfig::default();
        cfg.connections.push(Connection {
            id: "c".into(),
            preset: PresetId::OpenAi,
            base_url: "http://127.0.0.1:9/v1".into(),
            revision: 1,
        });
        let slot = |model: &str| {
            Some(SlotConfig {
                connection: "c".into(),
                model: model.into(),
                checked: 1,
                speech_model: None,
            })
        };
        cfg.stt = slot("whisper-1");
        cfg.chat = slot("gpt-test");
        cfg.tts = slot("nova");
        cfg.chat_override.delegation = Some(delegation);
        Arc::new(Pipeline::build(&cfg, &Secrets::default(), || true).unwrap())
    }

    /// The tools of a brain with the owner's MCP servers `(name, address)`.
    fn owner(
        servers: &[(&str, &str)],
        delegation: Delegation,
        switches: &[(&str, bool)],
        surface: Surface,
    ) -> DeviceTools {
        let mut live = AssistLive::empty();
        live.pipeline = Some(pipeline(delegation));
        live.switches = switches
            .iter()
            .map(|&(name, on)| (name.to_owned(), on))
            .collect();
        live.mcp = servers
            .iter()
            .enumerate()
            .map(|(n, &(name, url))| {
                Arc::new(OwnerMcp::new(
                    &format!("mcp-{n}"),
                    name,
                    Secret::new(url),
                    None,
                ))
            })
            .collect();
        let (inputs, _rx) = mpsc::channel(64);
        let status = watch::channel(Snapshot::default()).1;
        DeviceTools::new(
            &crate::tools::ToolKit::new(crate::metrics::Counters::default()),
            inputs,
            status,
            Arc::new(live),
            surface,
            Caller::Run(RunId(1)),
        )
    }

    #[tokio::test]
    async fn a_tool_loop_brain_gets_the_owner_tools_under_their_server_name() {
        let home = remote(&["lights_on", "volume"]).await;
        let set = remote(&["volume", "scene"]).await;
        let away = down().await;
        let tools = owner(
            &[("home", &home), ("set", &set), ("away", &away)],
            Delegation::ToolLoop,
            &[],
            Surface::Run,
        );

        let specs = tools.specs().await;

        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        for name in ["home_lights_on", "home_volume", "set_scene", "set_volume"] {
            assert!(names.contains(&name), "{name}: {names:?}");
        }
        // A device tool's name stays the device tool's.
        assert_eq!(names.iter().filter(|n| **n == "set_volume").count(), 1);
        let set_volume = specs.iter().find(|s| s.name == "set_volume").unwrap();
        assert_eq!(set_volume.description, "Set the speaker volume.");
        // A server that is down is left out.
        assert!(!names.iter().any(|n| n.starts_with("away_")), "{names:?}");
        let lights = specs.iter().find(|s| s.name == "home_lights_on").unwrap();
        assert_eq!(lights.description, "Remote lights_on.");

        // A call reaches the server under the tool's own name.
        assert_eq!(
            tools.call("home_lights_on", json!({})).await,
            ToolOutput::ok("remote lights_on ran")
        );
        assert_eq!(
            tools.call("set_scene", json!({})).await,
            ToolOutput::ok("remote scene ran")
        );
        for name in ["away_scene", "home_scene", "lights_on"] {
            assert_eq!(
                tools.call(name, json!({})).await,
                ToolOutput::error(format!("Unknown tool {name}."))
            );
        }

        // Each call counts as the server that answered it.
        let text = tools.kit.counters.tool_calls_text();
        for line in [
            "pinna_tool_calls_total{tool=\"home\",outcome=\"ok\",via=\"brain\"} 1",
            "pinna_tool_calls_total{tool=\"set\",outcome=\"ok\",via=\"brain\"} 1",
            "pinna_tool_calls_total{tool=\"other\",outcome=\"error\",via=\"brain\"} 3",
        ] {
            assert!(text.lines().any(|l| l == line), "no {line:?} in\n{text}");
        }
    }

    #[tokio::test]
    async fn a_hand_off_brain_and_mcp_clients_get_no_owner_tools() {
        let home = remote(&["lights_on"]).await;
        for (delegation, surface) in [
            (Delegation::HandOff, Surface::Run),
            (Delegation::ToolLoop, Surface::Mcp),
        ] {
            let tools = owner(&[("home", &home)], delegation, &[], surface);

            let names = names(&tools).await;

            assert!(names.contains(&"set_volume".to_owned()));
            assert!(!names.iter().any(|n| n.starts_with("home_")), "{names:?}");
            assert_eq!(
                tools.call("home_lights_on", json!({})).await,
                ToolOutput::error("Unknown tool home_lights_on.")
            );
        }
    }

    #[tokio::test]
    async fn a_switched_off_owner_tool_is_absent_and_refused() {
        let home = remote(&["lights_on", "lights_off"]).await;
        let tools = owner(
            &[("home", &home)],
            Delegation::ToolLoop,
            &[("home_lights_on", false)],
            Surface::Run,
        );

        let names = names(&tools).await;

        assert!(!names.contains(&"home_lights_on".to_owned()));
        assert!(names.contains(&"home_lights_off".to_owned()));
        assert_eq!(
            tools.call("home_lights_on", json!({})).await,
            ToolOutput::error("The owner switched home_lights_on off under Assistant.")
        );
    }

    #[tokio::test]
    async fn the_weather_tool_searches_a_named_place() {
        use pinna_assist::testing::{FakeResponse, FakeServer};
        use pinna_http::reqwest::{Method, StatusCode};

        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            "/v1/search?name=Paris%2C+France&count=1&language=en&format=json",
            FakeResponse::new(StatusCode::OK, r#"{"generationtime_ms": 0.4}"#),
        );
        let (inputs, _rx) = mpsc::channel(64);
        let (_tx, status) = watch::channel(Snapshot::default());
        let brain = DeviceTools::new(
            &crate::tools::ToolKit::new(crate::metrics::Counters::default())
                .with_weather(weather::tests::open_meteo(&server)),
            inputs,
            status,
            Arc::new(AssistLive::empty()),
            Surface::Run,
            Caller::Mcp,
        );
        let out = brain
            .call("weather", json!({"place": "Paris, France"}))
            .await;
        assert!(out.is_error);
        assert!(
            out.text.starts_with("No place called Paris, France"),
            "{out:?}"
        );
        assert_eq!(server.requests().len(), 1, "the named place is searched");
    }

    #[tokio::test]
    async fn time_names_the_zone_and_weather_is_for_the_brain_only() {
        let (inputs, _rx) = mpsc::channel(64);
        let (_tx, status) = watch::channel(Snapshot::default());
        let live = Arc::new(AssistLive::empty());
        let brain = DeviceTools::new(
            &crate::tools::ToolKit::new(crate::metrics::Counters::default()),
            inputs.clone(),
            status.clone(),
            live.clone(),
            Surface::Run,
            Caller::Mcp,
        );
        let time = brain.call("time", json!({})).await;
        assert!(!time.is_error && time.text.ends_with("(UTC)"), "{time:?}");
        // No place is set: the tool says so instead of guessing one.
        assert!(brain.call("weather", json!({})).await.is_error);

        let mcp = DeviceTools::new(
            &crate::tools::ToolKit::new(crate::metrics::Counters::default()),
            inputs,
            status,
            live,
            Surface::Mcp,
            Caller::Mcp,
        );
        let names: Vec<String> = mcp.specs().await.into_iter().map(|s| s.name).collect();
        assert!(names.contains(&"time".to_owned()));
        assert!(!names.contains(&weather::NAME.to_owned()));
    }

    /// Tools over a bare channel with an updater, as `main` builds them.
    fn with_updater(
        surface: Surface,
        updater: Option<Arc<Updater>>,
        switches: &[(&str, bool)],
    ) -> DeviceTools {
        let (inputs, _rx) = mpsc::channel(64);
        let mut kit = ToolKit::new(crate::metrics::Counters::default());
        if let Some(updater) = updater {
            kit = kit.with_updater(updater);
        }
        DeviceTools::new(
            &kit,
            inputs,
            watch::channel(Snapshot::default()).1,
            live(switches),
            surface,
            match surface {
                Surface::Run => Caller::Run(RunId(1)),
                Surface::Mcp => Caller::Mcp,
            },
        )
    }

    #[tokio::test]
    async fn the_update_tools_are_offered_on_both_surfaces() {
        for tool in ["updates", "check_for_updates"] {
            let info = catalogue().find(|t| t.name == tool).unwrap();
            assert!(info.brain && info.mcp, "{tool}");
            for surface in [Surface::Run, Surface::Mcp] {
                assert!(
                    names(&with_updater(surface, None, &[]))
                        .await
                        .contains(&tool.to_owned())
                );
            }
        }
    }

    #[tokio::test]
    async fn the_update_tools_say_when_there_is_no_updater() {
        let tools = with_updater(Surface::Mcp, None, &[]);
        for tool in ["updates", "check_for_updates"] {
            let out = tools.call(tool, json!({})).await;
            assert!(out.is_error, "{tool}");
            assert_eq!(out.text, update::NO_UPDATER);
        }
    }

    #[tokio::test]
    async fn checking_is_refused_with_the_updaters_sentence_while_updates_are_off() {
        let dir = tempfile::tempdir().unwrap();
        let updater = update::rigged_updater(dir.path(), "2.0.0", UpdateChannel::Off);
        let tools = with_updater(Surface::Run, Some(updater), &[]);

        let out = tools.call("check_for_updates", json!({})).await;

        assert_eq!(
            out,
            ToolOutput::error("Updates are off. Choose stable or beta to check for releases.")
        );
        let state = tools.call("updates", json!({})).await;
        assert!(!state.is_error);
        assert!(state.text.contains("\"channel\":\"off\""), "{}", state.text);
    }

    #[tokio::test]
    async fn a_check_through_the_tool_answers_with_the_release() {
        let dir = tempfile::tempdir().unwrap();
        let updater = update::rigged_updater(dir.path(), "2.0.0", UpdateChannel::Stable);
        let tools = with_updater(Surface::Mcp, Some(updater), &[]);

        let out = tools.call("check_for_updates", json!({})).await;

        assert!(!out.is_error, "{}", out.text);
        let shown: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(shown["release"]["version"], "2.0.0");
        assert_eq!(shown["release"]["action"], "install");
        assert_eq!(shown["channel"], "stable");
        // The rig boots 1.0.0 from slot a, as a satellite would.
        assert_eq!(shown["version"], "1.0.0");
        assert_eq!(shown["slot"], "a");
        assert!(shown.get("unavailable").is_none(), "{shown}");
        assert!(!out.text.contains("://"), "{}", out.text);
    }

    #[tokio::test]
    async fn install_update_refuses_what_install_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let updater = update::rigged_updater(dir.path(), "0.9.0", UpdateChannel::Stable);
        let tools = with_updater(
            Surface::Run,
            Some(updater.clone()),
            &[("install_update", true)],
        );

        // Before any check there is nothing to install.
        assert_eq!(
            tools
                .call("install_update", json!({"version": "0.9.0"}))
                .await,
            ToolOutput::error("No release to install: check for updates first.")
        );

        // Never an older version: going back stays on the dashboard.
        assert!(!tools.call("check_for_updates", json!({})).await.is_error);
        assert_eq!(
            tools
                .call("install_update", json!({"version": "0.9.0"}))
                .await,
            ToolOutput::error("0.9.0 is older than the running 1.0.0. Confirm going back first.")
        );
        let after = updater.get_updates().await;
        assert!(after.last_install.as_option().is_none() && !after.installing);
        assert!(!updater.restart_pending());
    }

    #[tokio::test]
    async fn only_install_update_starts_off_and_only_the_brain_gets_it() {
        for info in catalogue() {
            assert_eq!(
                info.off_by_default,
                info.name == "install_update",
                "{}",
                info.name
            );
        }
        let info = catalogue().find(|t| t.name == "install_update").unwrap();
        assert!(info.brain && !info.mcp);
    }

    #[tokio::test]
    async fn install_update_is_refused_until_the_owner_turns_it_on() {
        let dir = tempfile::tempdir().unwrap();
        let updater = update::rigged_updater(dir.path(), "2.0.0", UpdateChannel::Stable);
        let run = with_updater(Surface::Run, Some(updater.clone()), &[]);
        assert!(!names(&run).await.contains(&"install_update".to_owned()));
        assert!(names(&run).await.contains(&"updates".to_owned()));

        let out = run
            .call("install_update", json!({"version": "2.0.0"}))
            .await;

        assert_eq!(
            out,
            ToolOutput::error(
                "install_update is off until the owner turns it on under Assistant, Tools."
            )
        );
        // Turned on and off again: the owner switched it off.
        let off = with_updater(Surface::Run, Some(updater), &[("install_update", false)]);
        assert_eq!(
            off.call("install_update", json!({"version": "2.0.0"}))
                .await,
            ToolOutput::error("The owner switched install_update off under Assistant.")
        );
    }

    #[tokio::test]
    async fn install_update_is_never_offered_over_mcp() {
        let dir = tempfile::tempdir().unwrap();
        let updater = update::rigged_updater(dir.path(), "2.0.0", UpdateChannel::Stable);
        let on = [("install_update", true)];
        let run = with_updater(Surface::Run, Some(updater.clone()), &on);
        let mcp = with_updater(Surface::Mcp, Some(updater), &on);

        assert!(names(&run).await.contains(&"install_update".to_owned()));
        assert!(!names(&mcp).await.contains(&"install_update".to_owned()));
        assert_eq!(
            mcp.call("install_update", json!({"version": "2.0.0"}))
                .await,
            ToolOutput::error("Unknown tool install_update.")
        );
    }

    #[tokio::test]
    async fn install_update_installs_the_found_release_and_restarts_once_idle() {
        let dir = tempfile::tempdir().unwrap();
        let updater = update::rigged_updater(dir.path(), "2.0.0", UpdateChannel::Stable);
        let tools = with_updater(
            Surface::Run,
            Some(updater.clone()),
            &[("install_update", true)],
        );
        assert!(!tools.call("check_for_updates", json!({})).await.is_error);

        // Not the version the check found.
        let out = tools
            .call("install_update", json!({"version": "2.0.1"}))
            .await;
        assert!(
            out.is_error && out.text.contains("found 2.0.0"),
            "{}",
            out.text
        );
        assert!(!updater.restart_pending());

        let out = tools
            .call("install_update", json!({"version": " 2.0.0 "}))
            .await;

        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.starts_with("Installing 2.0.0."), "{}", out.text);
        for _ in 0..100 {
            if updater.restart_pending() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("the install did not leave a restart pending");
    }

    #[tokio::test]
    async fn every_call_counts_once_by_tool_and_surface() {
        let counters = Counters::default();
        let kit = ToolKit::new(counters.clone());
        let (inputs, _rx) = mpsc::channel(64);
        let (_tx, status) = watch::channel(Snapshot::default());
        let tools = DeviceTools::new(
            &kit,
            inputs,
            status,
            Arc::new(AssistLive::empty()),
            Surface::Run,
            Caller::Mcp,
        );
        assert!(!tools.call("time", json!({})).await.is_error);
        assert!(tools.call("no_such_tool", json!({})).await.is_error);
        let text = counters.tool_calls_text();
        for line in [
            "pinna_tool_calls_total{tool=\"time\",outcome=\"ok\",via=\"brain\"} 1",
            "pinna_tool_calls_total{tool=\"other\",outcome=\"error\",via=\"brain\"} 1",
        ] {
            assert!(text.lines().any(|l| l == line), "no {line:?} in\n{text}");
        }
    }
}
