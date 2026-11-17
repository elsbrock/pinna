//! The tools the brain and MCP clients call (ADR 0017).
//!
//! One table names every tool, the surfaces that offer it and its
//! arguments. The owner switches tools off per name in standalone.toml; a
//! tool that is off is left out of the list and refused when called anyway.

mod device;
pub mod weather;

use std::sync::Arc;

use futures::future::BoxFuture;
use pinna_mcp::{ToolOutput, ToolSet, ToolSpec};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot, watch};

use crate::leds::LightEffect;
use crate::standalone::AssistLive;
use crate::voice::{ApiCommand, Input, RunId, Snapshot};
use device::Op;
use weather::OpenMeteo;

/// Free Brain queue slots an MCP call leaves for the wake word and buttons.
const BRAIN_RESERVE: usize = 16;
const BUSY: &str = "The satellite is busy. Try again in a moment.";
const SHUTTING_DOWN: &str = "pinnad is shutting down.";

/// Where a tool list is offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// The brain's tool loop in a standalone run.
    Brain,
    /// Pinna's MCP server.
    // The `/mcp` handler (T67) offers this surface.
    #[allow(dead_code)]
    Mcp,
}

/// Who calls a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    /// A standalone run: the Brain applies the command only while the run
    /// is current.
    Run(RunId),
    /// An MCP client.
    // The `/mcp` handler (T67) calls as one.
    #[allow(dead_code)]
    Mcp,
}

/// The surfaces that offer a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    Both,
    BrainOnly,
    McpOnly,
}

impl Reach {
    fn offers(self, surface: Surface) -> bool {
        matches!(
            (self, surface),
            (Reach::Both, _) | (Reach::BrainOnly, Surface::Brain) | (Reach::McpOnly, Surface::Mcp)
        )
    }
}

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
}

const PERCENT: Kind = Kind::Integer { min: 0, max: 100 };

static TOOLS: &[Tool] = &[
    Tool {
        name: "set_volume",
        reach: Reach::Both,
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
        about: "Pause the music or radio.",
        params: &[],
        op: Op::PauseMedia,
    },
    Tool {
        name: "resume_media",
        reach: Reach::Both,
        about: "Resume paused music or radio.",
        params: &[],
        op: Op::ResumeMedia,
    },
    Tool {
        name: "stop_media",
        reach: Reach::Both,
        about: "Stop the music or radio.",
        params: &[],
        op: Op::StopMedia,
    },
    Tool {
        name: "clear_notification",
        reach: Reach::Both,
        about: "Clear the notification the light ring shows.",
        params: &[],
        op: Op::ClearNotification,
    },
    Tool {
        name: "mute_microphone",
        reach: Reach::Both,
        about: "Mute the microphone. Only a person can unmute it.",
        params: &[],
        op: Op::MuteMicrophone,
    },
    Tool {
        name: "set_timer",
        reach: Reach::Both,
        about: "Start a timer that rings on this satellite.",
        params: &[
            Param {
                name: "seconds",
                kind: Kind::Integer {
                    min: 1,
                    max: 86_400,
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
        about: "List the timers with the seconds each has left.",
        params: &[],
        op: Op::ListTimers,
    },
    Tool {
        name: "cancel_timer",
        reach: Reach::Both,
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
        about: "The current local date and time where the satellite is.",
        params: &[],
        op: Op::Time,
    },
    Tool {
        name: weather::NAME,
        reach: Reach::BrainOnly,
        about: "The weather now and for the next two days at the satellite's place.",
        params: &[],
        op: Op::Weather,
    },
    Tool {
        name: "cancel_request",
        reach: Reach::McpOnly,
        about: "Stop the request the satellite is handling, or silence a ringing alarm.",
        params: &[],
        op: Op::CancelRequest,
    },
    Tool {
        name: "status",
        reach: Reach::McpOnly,
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
    weather: OpenMeteo,
}

impl DeviceTools {
    pub fn new(
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
            weather: OpenMeteo::default(),
        }
    }

    /// The owner's switch for a tool; a tool without one is on.
    fn switched_on(&self, name: &str) -> bool {
        self.live.switches.get(name).copied().unwrap_or(true)
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
            Caller::Mcp => {
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
}

impl ToolSet for DeviceTools {
    fn specs(&self) -> BoxFuture<'_, Vec<ToolSpec>> {
        let specs = TOOLS
            .iter()
            .filter(|t| t.reach.offers(self.surface) && self.switched_on(t.name))
            .map(Tool::spec)
            .collect();
        Box::pin(std::future::ready(specs))
    }

    fn call<'a>(&'a self, name: &'a str, args: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let Some(tool) = TOOLS
                .iter()
                .find(|t| t.name == name && t.reach.offers(self.surface))
            else {
                return ToolOutput::error(format!("Unknown tool {name}."));
            };
            if !self.switched_on(name) {
                return ToolOutput::error(format!(
                    "The owner switched {name} off under Assistant."
                ));
            }
            let result = match Args::parse(tool, args) {
                Ok(args) => device::run(self, tool.op, &args).await,
                Err(why) => Err(why),
            };
            match result {
                Ok(text) => ToolOutput::ok(text),
                Err(why) => ToolOutput::error(why),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use pinna_esphome::{DeviceEvent, EsphomeHandle};

    use super::*;
    use crate::leds::LedState;
    use crate::settings::{Mode, Settings};
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
                    },
                    mdns: None,
                    encrypted: false,
                    has_hw_privacy: false,
                    airplay: false,
                    bluetooth: false,
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

        /// As an MCP client: the Brain refuses a run's commands until T28.
        fn tools(&self, surface: Surface, switches: &[(&str, bool)]) -> DeviceTools {
            DeviceTools::new(
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
        let tools = DeviceTools::new(inputs, status, live(&[]), surface, caller);
        (tools, rx)
    }

    async fn names(tools: &DeviceTools) -> Vec<String> {
        tools.specs().await.into_iter().map(|s| s.name).collect()
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
        for surface in [Surface::Brain, Surface::Mcp] {
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
    async fn a_non_http_address_is_refused_before_the_brain() {
        let (tools, mut rx) = bare(Surface::Brain, Caller::Run(RunId(1)));

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
        for surface in [Surface::Brain, Surface::Mcp] {
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
        let (brain, _rx) = bare(Surface::Brain, Caller::Run(RunId(1)));
        let (mcp, _rx) = bare(Surface::Mcp, Caller::Mcp);
        let brain_names = names(&brain).await;
        let mcp_names = names(&mcp).await;
        for name in ["speak", "cancel_request", "status"] {
            assert!(!brain_names.iter().any(|n| n == name), "{name}");
        }
        assert!(!mcp_names.iter().any(|n| n == "weather"));
        for name in ["cancel_request", "status", "set_timer", "mute_microphone"] {
            assert!(mcp_names.iter().any(|n| n == name), "{name}");
        }
        let out = brain.call("cancel_request", json!({})).await;
        assert_eq!(out, ToolOutput::error("Unknown tool cancel_request."));
        assert!(!Reach::BrainOnly.offers(Surface::Mcp));
        assert!(!Reach::McpOnly.offers(Surface::Brain));
        assert!(Reach::Both.offers(Surface::Brain) && Reach::Both.offers(Surface::Mcp));
    }

    #[tokio::test]
    async fn a_run_sends_its_commands_fenced_by_its_id() {
        let (tools, mut rx) = bare(Surface::Brain, Caller::Run(RunId(7)));
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
        let (tools, mut rx) = bare(Surface::Brain, Caller::Run(RunId(2)));
        let call = tokio::spawn(async move { tools.call("mute_microphone", json!({})).await });

        match rx.recv().await.unwrap() {
            Input::RunTool {
                run: RunId(2),
                cmd: ApiCommand::SetMute(true, reply),
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
    async fn mcp_leaves_the_last_queue_slots_to_the_device() {
        let (inputs, _rx) = mpsc::channel(64);
        while inputs.capacity() >= BRAIN_RESERVE {
            inputs.try_send(Input::Privacy(false)).unwrap();
        }
        let status = watch::channel(Snapshot::default()).1;
        let tools = DeviceTools::new(inputs.clone(), status, live(&[]), Surface::Mcp, Caller::Mcp);

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
        let (brain, _rx) = bare(Surface::Brain, Caller::Run(RunId(1)));
        specs.extend(brain.specs().await);
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

    #[tokio::test]
    async fn time_names_the_zone_and_weather_is_for_the_brain_only() {
        let (inputs, _rx) = mpsc::channel(64);
        let (_tx, status) = watch::channel(Snapshot::default());
        let live = Arc::new(AssistLive::empty());
        let brain = DeviceTools::new(
            inputs.clone(),
            status.clone(),
            live.clone(),
            Surface::Brain,
            Caller::Mcp,
        );
        let time = brain.call("time", json!({})).await;
        assert!(!time.is_error && time.text.ends_with("(UTC)"), "{time:?}");
        // No place is set: the tool says so instead of guessing one.
        assert!(brain.call("weather", json!({})).await.is_error);

        let mcp = DeviceTools::new(inputs, status, live, Surface::Mcp, Caller::Mcp);
        let names: Vec<String> = mcp.specs().await.into_iter().map(|s| s.name).collect();
        assert!(names.contains(&"time".to_owned()));
        assert!(!names.contains(&weather::NAME.to_owned()));
    }
}
