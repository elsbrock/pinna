//! Stream a brain reply through the tool loop and bounded synthesis queue.

use std::{path::Path, time::Duration};

use futures_core::future::BoxFuture;
use futures_util::{StreamExt, stream::FuturesOrdered};
use pinna_mcp::{ToolOutput, ToolSet};
use tokio::time::Instant;

use crate::{
    chat::{Chat, ChatDelta, ChatRequest, Message},
    conversation::Turn,
    error::{ProviderError, Reason, Slot},
    harness::{Harness, Out, Step},
    observe::{Guard, Observer, Spend},
    presets::{Behaviour, Delegation, PresetId},
    reply::{AudioBody, AudioKind, Credit, ReplySink, SpeechAudio},
    run::{RunEvent, RunEvents},
    tts::Tts,
};

const MAX_TOOLS: usize = 64;

pub(crate) struct ReplyRequest<'a> {
    pub question: String,
    pub history: &'a [Turn],
    pub system: &'a str,
    pub user_id: Option<&'a str>,
    pub chat: &'a dyn Chat,
    pub tts: &'a dyn Tts,
    pub tools: &'a dyn ToolSet,
    pub behaviour: &'a Behaviour,
    pub chat_preset: PresetId,
    pub tts_preset: PresetId,
    pub sink: &'a ReplySink,
    pub ack: Option<&'a Path>,
    pub observer: &'a dyn Observer,
}

#[derive(Debug)]
pub(crate) struct ReplyOutcome {
    pub turn: Option<Turn>,
    pub spoke: bool,
}

struct Synthesized {
    audio: SpeechAudio,
    credit: Credit,
}

type Synthesis<'a> = BoxFuture<'a, Result<Synthesized, ProviderError>>;

struct Driver<'a> {
    tts: &'a dyn Tts,
    tts_preset: PresetId,
    tts_timeout: Duration,
    chat_preset: PresetId,
    sink: &'a ReplySink,
    observer: &'a dyn Observer,
    events: &'a dyn RunEvents,
    reply_ready: bool,
    spoke: bool,
}

impl<'a> Driver<'a> {
    fn synthesize(&self, sentence: String) -> Synthesis<'a> {
        let sink = self.sink;
        let tts = self.tts;
        let observer = self.observer;
        let preset = self.tts_preset;
        let timeout = self.tts_timeout;
        Box::pin(async move {
            let credit = sink
                .credit()
                .await
                .map_err(|_| ProviderError::new(Slot::Tts, preset, Reason::Internal))?;
            let started = Instant::now();
            let mut audio = match tokio::time::timeout(timeout, tts.speak(&sentence)).await {
                Ok(result) => result?,
                Err(_) => {
                    return Err(ProviderError::new(Slot::Tts, preset, Reason::Timeout));
                }
            };
            observer.stage(Slot::Tts, started.elapsed().as_secs_f64());
            observer.spent(preset, Spend::Characters(sentence.chars().count() as u64));
            audio.kind = AudioKind::Sentence;
            Ok(Synthesized { audio, credit })
        })
    }

    async fn send(
        &mut self,
        audio: SpeechAudio,
        credit: Option<Credit>,
    ) -> Result<(), ProviderError> {
        let kind = audio.kind;
        if !self.reply_ready {
            self.events.emit(RunEvent::ReplyReady).await;
            self.reply_ready = true;
        }
        self.sink
            .send(audio, credit)
            .await
            .map_err(|_| ProviderError::new(Slot::Tts, self.tts_preset, Reason::Internal))?;
        self.spoke |= kind == AudioKind::Sentence;
        Ok(())
    }

    async fn acknowledge(&mut self, path: &Path) -> Result<(), ProviderError> {
        self.send(
            SpeechAudio {
                kind: AudioKind::Ack,
                mime: None,
                body: AudioBody::File(path.to_path_buf()),
            },
            None,
        )
        .await?;
        self.observer.guard(Guard::Ack);
        Ok(())
    }
}

pub(crate) async fn reply(
    request: ReplyRequest<'_>,
    events: &dyn RunEvents,
) -> Result<ReplyOutcome, ProviderError> {
    let limits = &request.behaviour.limits;
    let tool_loop = request.behaviour.delegation == Delegation::ToolLoop;
    let specs = if tool_loop {
        request
            .tools
            .specs()
            .await
            .into_iter()
            .take(MAX_TOOLS)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut harness = Harness::new(request.question, limits, tool_loop);
    let mut driver = Driver {
        tts: request.tts,
        tts_preset: request.tts_preset,
        tts_timeout: limits.tts_first_audio,
        chat_preset: request.chat_preset,
        sink: request.sink,
        observer: request.observer,
        events,
        reply_ready: false,
        spoke: false,
    };
    let mut synthesis = FuturesOrdered::new();
    let mut round = 1_u8;
    let mut first_text_seen = false;
    let mut ack_sent = false;
    let ack_enabled = limits.ack_after.is_some() && request.ack.is_some();

    loop {
        let mut messages = history_messages(request.history);
        messages.extend_from_slice(harness.messages());
        let offered = if tool_loop && round < limits.rounds {
            specs.as_slice()
        } else {
            &[]
        };
        let chat_request = ChatRequest {
            system: request.system,
            messages: &messages,
            tools: offered,
            max_tokens: limits.max_tokens,
            user: if request.behaviour.stable_user {
                request.user_id
            } else {
                None
            },
        };
        let started = Instant::now();
        let start = async {
            let mut stream = request.chat.stream(chat_request).await?;
            let first = stream.next().await.transpose()?;
            Ok::<_, ProviderError>((stream, first))
        };
        tokio::pin!(start);
        let mut deadline = Box::pin(tokio::time::sleep(limits.first_text));
        let mut ack_timer = Box::pin(tokio::time::sleep(
            limits.ack_after.unwrap_or(limits.first_text),
        ));
        let (mut stream, mut next_delta) = loop {
            tokio::select! {
                result = &mut start => break result?,
                _ = &mut deadline => {
                    return Err(ProviderError::new(Slot::Chat, request.chat_preset, Reason::Timeout));
                }
                _ = &mut ack_timer, if !ack_sent && ack_enabled => {
                    driver.acknowledge(request.ack.expect("checked above")).await?;
                    ack_sent = true;
                }
            }
        };

        // The brain may pause between pieces of its answer for as long as it
        // may take to start one; a stream that stalls after its first text
        // fails here, as the brain's fault, before the Brain's backstop.
        let mut idle = Box::pin(tokio::time::sleep(limits.first_text));
        loop {
            let Some(delta) = next_delta.take() else {
                return Err(ProviderError::new(
                    Slot::Chat,
                    request.chat_preset,
                    Reason::Protocol,
                ));
            };
            if matches!(delta, ChatDelta::Text(_)) && !first_text_seen {
                first_text_seen = true;
                request
                    .observer
                    .stage(Slot::Chat, started.elapsed().as_secs_f64());
                events.emit(RunEvent::Progress).await;
            }
            let end = matches!(delta, ChatDelta::End);
            for out in harness.on_delta(delta) {
                queue_out(&driver, &mut synthesis, out);
            }
            if end {
                break;
            }

            enum Next {
                Delta(Option<Result<ChatDelta, ProviderError>>),
                Audio(Option<Result<Synthesized, ProviderError>>),
                Ack,
                Stalled,
            }
            idle.as_mut()
                .reset(tokio::time::Instant::now() + limits.first_text);
            let next = loop {
                let next = tokio::select! {
                    next = stream.next() => Next::Delta(next),
                    audio = synthesis.next(), if !synthesis.is_empty() => Next::Audio(audio),
                    _ = &mut ack_timer, if !ack_sent && !first_text_seen && ack_enabled => Next::Ack,
                    _ = &mut idle => Next::Stalled,
                };
                match next {
                    Next::Delta(next) => break next,
                    Next::Stalled => {
                        return Err(ProviderError::new(
                            Slot::Chat,
                            request.chat_preset,
                            Reason::Timeout,
                        ));
                    }
                    Next::Audio(Some(audio)) => {
                        let item = audio?;
                        driver.send(item.audio, Some(item.credit)).await?;
                    }
                    Next::Audio(None) => {
                        return Err(ProviderError::new(
                            Slot::Tts,
                            request.tts_preset,
                            Reason::Protocol,
                        ));
                    }
                    Next::Ack => {
                        driver
                            .acknowledge(request.ack.expect("checked above"))
                            .await?;
                        ack_sent = true;
                    }
                }
            };
            next_delta = next.transpose()?;
        }
        drop(stream);

        let step = harness.on_stream_end();
        for out in harness.drain() {
            queue_out(&driver, &mut synthesis, out);
        }
        while let Some(item) = synthesis.next().await {
            let item = item?;
            driver.send(item.audio, Some(item.credit)).await?;
        }

        match step {
            Step::Done { .. } => {
                return Ok(ReplyOutcome {
                    turn: harness.turn(),
                    spoke: driver.spoke,
                });
            }
            Step::Fail(reason) => {
                return Err(ProviderError::new(Slot::Chat, request.chat_preset, reason));
            }
            Step::CallTools(calls) => {
                let mut results = Vec::with_capacity(calls.len());
                for call in calls {
                    let output = match tokio::time::timeout(
                        limits.tool,
                        request.tools.call(&call.name, call.args.clone()),
                    )
                    .await
                    {
                        Ok(output) => output,
                        Err(_) => ToolOutput::error("The tool timed out."),
                    };
                    results.push((call, output));
                }
                harness.on_results(results);
                events.emit(RunEvent::Progress).await;
                round = round.saturating_add(1);
            }
        }
    }
}

fn history_messages(history: &[Turn]) -> Vec<Message> {
    let mut messages = Vec::with_capacity(history.len().saturating_mul(2));
    for turn in history {
        messages.push(Message::User(turn.user.clone()));
        messages.push(Message::Assistant {
            text: turn.assistant.clone(),
            calls: Vec::new(),
            signature: None,
        });
    }
    messages
}

fn queue_out<'a>(driver: &Driver<'a>, synthesis: &mut FuturesOrdered<Synthesis<'a>>, out: Out) {
    match out {
        Out::Sentence(sentence) => synthesis.push_back(driver.synthesize(sentence)),
        Out::Usage { input, output } => driver
            .observer
            .spent(driver.chat_preset, Spend::Tokens { input, output }),
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{Mutex, MutexGuard},
    };

    use bytes::Bytes;
    use futures_util::stream;
    use pinna_mcp::ToolSpec;
    use serde_json::Value;

    use super::*;

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().expect("test fake mutex")
    }

    #[derive(Clone, Debug)]
    pub(crate) struct RecordedRequest {
        pub messages: Vec<Message>,
        pub tools: usize,
        pub max_tokens: u32,
        pub user: Option<String>,
    }

    enum Round {
        Now(Vec<ChatDelta>),
        Delayed(Duration, Vec<ChatDelta>),
        PendingAfter(Vec<ChatDelta>),
        Pending,
    }

    pub(crate) struct ScriptedChat {
        rounds: Mutex<VecDeque<Round>>,
        requests: Mutex<Vec<RecordedRequest>>,
    }

    impl ScriptedChat {
        pub(crate) fn new(rounds: impl IntoIterator<Item = Vec<ChatDelta>>) -> Self {
            Self {
                rounds: Mutex::new(rounds.into_iter().map(Round::Now).collect()),
                requests: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn delayed(delay: Duration, deltas: Vec<ChatDelta>) -> Self {
            Self {
                rounds: Mutex::new(VecDeque::from([Round::Delayed(delay, deltas)])),
                requests: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn pending_after(deltas: Vec<ChatDelta>) -> Self {
            Self {
                rounds: Mutex::new(VecDeque::from([Round::PendingAfter(deltas)])),
                requests: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn pending() -> Self {
            Self {
                rounds: Mutex::new(VecDeque::from([Round::Pending])),
                requests: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn requests(&self) -> Vec<RecordedRequest> {
            lock(&self.requests).clone()
        }
    }

    impl Chat for ScriptedChat {
        fn stream<'a>(
            &'a self,
            req: ChatRequest<'a>,
        ) -> BoxFuture<
            'a,
            Result<
                futures_core::stream::BoxStream<'a, Result<ChatDelta, ProviderError>>,
                ProviderError,
            >,
        > {
            lock(&self.requests).push(RecordedRequest {
                messages: req.messages.to_vec(),
                tools: req.tools.len(),
                max_tokens: req.max_tokens,
                user: req.user.map(str::to_owned),
            });
            let round = lock(&self.rounds).pop_front().expect("scripted chat round");
            Box::pin(async move {
                let deltas = match round {
                    Round::Now(deltas) => deltas,
                    Round::Delayed(delay, deltas) => {
                        tokio::time::sleep(delay).await;
                        deltas
                    }
                    Round::PendingAfter(deltas) => {
                        return Ok(stream::iter(deltas.into_iter().map(Ok))
                            .chain(stream::pending())
                            .boxed());
                    }
                    Round::Pending => std::future::pending().await,
                };
                Ok(stream::iter(deltas.into_iter().map(Ok)).boxed())
            })
        }
    }

    #[derive(Default)]
    pub(crate) struct ScriptedTts {
        delays: BTreeMap<String, Duration>,
        calls: Mutex<Vec<String>>,
    }

    impl ScriptedTts {
        pub(crate) fn delayed(delays: &[(&str, Duration)]) -> Self {
            Self {
                delays: delays
                    .iter()
                    .map(|(text, delay)| ((*text).to_owned(), *delay))
                    .collect(),
                calls: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            lock(&self.calls).clone()
        }
    }

    impl Tts for ScriptedTts {
        fn speak<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<SpeechAudio, ProviderError>> {
            let text = text.to_owned();
            lock(&self.calls).push(text.clone());
            let delay = self.delays.get(&text).copied().unwrap_or_default();
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                let bytes = Bytes::from(text.clone());
                Ok(SpeechAudio {
                    kind: AudioKind::Sentence,
                    mime: Some(text),
                    body: AudioBody::Stream(Box::pin(stream::once(async move {
                        Ok::<_, std::io::Error>(bytes)
                    }))),
                })
            })
        }
    }

    #[derive(Default)]
    pub(crate) struct TestEvents {
        events: Mutex<Vec<RunEvent>>,
    }

    impl TestEvents {
        pub(crate) fn events(&self) -> Vec<RunEvent> {
            lock(&self.events).clone()
        }
    }

    impl RunEvents for TestEvents {
        fn emit(&self, event: RunEvent) -> BoxFuture<'_, ()> {
            lock(&self.events).push(event);
            Box::pin(async {})
        }
    }

    #[derive(Default)]
    pub(crate) struct TestObserver {
        pub stages: Mutex<Vec<(Slot, f64)>>,
        pub spends: Mutex<Vec<(PresetId, Spend)>>,
        pub guards: Mutex<Vec<Guard>>,
        pub failures: Mutex<Vec<ProviderError>>,
    }

    impl Observer for TestObserver {
        fn stage(&self, slot: Slot, seconds: f64) {
            lock(&self.stages).push((slot, seconds));
        }

        fn spent(&self, preset: PresetId, spend: Spend) {
            lock(&self.spends).push((preset, spend));
        }

        fn guard(&self, guard: Guard) {
            lock(&self.guards).push(guard);
        }

        fn failed(&self, error: &ProviderError) {
            lock(&self.failures).push(error.clone());
        }
    }

    pub(crate) struct NoTools;

    impl ToolSet for NoTools {
        fn specs(&self) -> BoxFuture<'_, Vec<ToolSpec>> {
            Box::pin(async { Vec::new() })
        }

        fn call<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, ToolOutput> {
            Box::pin(async { ToolOutput::error("No tool is available.") })
        }
    }

    pub(crate) struct SlowTool;

    impl ToolSet for SlowTool {
        fn specs(&self) -> BoxFuture<'_, Vec<ToolSpec>> {
            Box::pin(async {
                vec![ToolSpec {
                    name: "weather".into(),
                    description: "Weather".into(),
                    input_schema: serde_json::json!({"type": "object"}),
                }]
            })
        }

        fn call<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, ToolOutput> {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                ToolOutput::ok("late")
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use pinna_mcp::ToolCall;
    use serde_json::json;

    use super::{test_util::*, *};
    use crate::{limits::RunLimits, reply::reply_channel};

    fn behaviour(mut limits: RunLimits, delegation: Delegation) -> Behaviour {
        limits.first_text = Duration::from_millis(50);
        limits.tool = Duration::from_millis(10);
        limits.tts_first_audio = Duration::from_millis(100);
        Behaviour {
            delegation,
            limits,
            ..crate::presets::preset(PresetId::OpenAi).behaviour
        }
    }

    fn request<'a>(
        question: &str,
        chat: &'a ScriptedChat,
        tts: &'a ScriptedTts,
        tools: &'a dyn ToolSet,
        behaviour: &'a Behaviour,
        sink: &'a ReplySink,
        observer: &'a TestObserver,
    ) -> ReplyRequest<'a> {
        ReplyRequest {
            question: question.into(),
            history: &[],
            system: "Be brief.",
            user_id: None,
            chat,
            tts,
            tools,
            behaviour,
            chat_preset: PresetId::OpenAi,
            tts_preset: PresetId::OpenAi,
            sink,
            ack: None,
            observer,
        }
    }

    async fn receive(
        source: &mut crate::reply::ReplySource,
        count: usize,
    ) -> Vec<(AudioKind, Option<String>)> {
        let mut audio = Vec::new();
        for _ in 0..count {
            let item = source.recv().await.unwrap();
            source.started(item.kind);
            audio.push((item.kind, item.mime));
        }
        audio
    }

    #[tokio::test(start_paused = true)]
    async fn sentences_stay_in_order_when_synthesis_finishes_out_of_order() {
        let chat = ScriptedChat::new([vec![
            ChatDelta::Text("One. Two. Three.".into()),
            ChatDelta::End,
        ]]);
        let tts = ScriptedTts::delayed(&[
            ("One.", Duration::from_millis(30)),
            ("Two.", Duration::ZERO),
            ("Three.", Duration::from_millis(10)),
        ]);
        let behaviour = behaviour(RunLimits::MODEL, Delegation::ToolLoop);
        let (sink, mut source) = reply_channel(2);
        let events = TestEvents::default();
        let observer = TestObserver::default();
        let run = reply(
            request(
                "Question", &chat, &tts, &NoTools, &behaviour, &sink, &observer,
            ),
            &events,
        );
        let (outcome, audio) = tokio::join!(run, receive(&mut source, 3));
        assert!(outcome.unwrap().spoke);
        assert_eq!(
            audio,
            [
                (AudioKind::Sentence, Some("One.".into())),
                (AudioKind::Sentence, Some("Two.".into())),
                (AudioKind::Sentence, Some("Three.".into())),
            ]
        );
        assert_eq!(
            events
                .events()
                .iter()
                .filter(|event| matches!(event, RunEvent::ReplyReady))
                .count(),
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn only_two_sentences_synthesise_before_playback_starts() {
        let chat = ScriptedChat::new([vec![
            ChatDelta::Text("One. Two. Three.".into()),
            ChatDelta::End,
        ]]);
        let tts = ScriptedTts::default();
        let behaviour = behaviour(RunLimits::MODEL, Delegation::ToolLoop);
        let (sink, mut source) = reply_channel(2);
        let events = TestEvents::default();
        let observer = TestObserver::default();
        let run = reply(
            request(
                "Question", &chat, &tts, &NoTools, &behaviour, &sink, &observer,
            ),
            &events,
        );
        let consume = async {
            let first = source.recv().await.unwrap();
            let second = source.recv().await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
            assert_eq!(tts.calls(), ["One.", "Two."]);
            source.started(first.kind);
            let third = source.recv().await.unwrap();
            assert_eq!(tts.calls(), ["One.", "Two.", "Three."]);
            source.started(second.kind);
            source.started(third.kind);
        };
        let (outcome, ()) = tokio::join!(run, consume);
        outcome.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_tool_timeout_goes_back_to_the_brain_as_an_error() {
        let call = ToolCall {
            id: "call-1".into(),
            name: "weather".into(),
            args: json!({}),
        };
        let chat = ScriptedChat::new([
            vec![ChatDelta::Call(call), ChatDelta::End],
            vec![ChatDelta::Text("Done.".into()), ChatDelta::End],
        ]);
        let tts = ScriptedTts::default();
        let behaviour = behaviour(RunLimits::MODEL, Delegation::ToolLoop);
        let (sink, mut source) = reply_channel(2);
        let events = TestEvents::default();
        let observer = TestObserver::default();
        let run = reply(
            request(
                "Question", &chat, &tts, &SlowTool, &behaviour, &sink, &observer,
            ),
            &events,
        );
        let (outcome, _) = tokio::join!(run, receive(&mut source, 1));
        outcome.unwrap();
        let requests = chat.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().any(|message| matches!(
            message,
            Message::ToolResult { output, .. }
                if output.is_error && output.text == "The tool timed out."
        )));
        assert_eq!(requests[0].tools, 1);
        assert_eq!(requests[1].tools, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_text_limit_fails_the_brain_slot() {
        let chat = ScriptedChat::pending();
        let tts = ScriptedTts::default();
        let behaviour = behaviour(RunLimits::MODEL, Delegation::ToolLoop);
        let (sink, _source) = reply_channel(2);
        let events = TestEvents::default();
        let observer = TestObserver::default();
        let error = reply(
            request(
                "Question", &chat, &tts, &NoTools, &behaviour, &sink, &observer,
            ),
            &events,
        )
        .await
        .unwrap_err();
        assert_eq!(error.slot, Slot::Chat);
        assert_eq!(error.reason, Reason::Timeout);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_that_stalls_after_its_first_text_fails_the_brain_slot() {
        // An agent speaks one sentence and then hangs: the run gives up after
        // the first-text limit, as the brain's fault, not the speech's.
        let chat = ScriptedChat::pending_after(vec![ChatDelta::Text("One moment. ".into())]);
        let tts = ScriptedTts::default();
        let behaviour = behaviour(RunLimits::AGENT, Delegation::HandOff);
        let (sink, _source) = reply_channel(2);
        let events = TestEvents::default();
        let observer = TestObserver::default();
        let started = tokio::time::Instant::now();
        let error = reply(
            request(
                "Question", &chat, &tts, &NoTools, &behaviour, &sink, &observer,
            ),
            &events,
        )
        .await
        .unwrap_err();
        assert_eq!((error.slot, error.reason), (Slot::Chat, Reason::Timeout));
        assert_eq!(started.elapsed(), behaviour.limits.first_text);
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_acknowledges_after_three_seconds_but_a_model_does_not() {
        for (delegation, agent) in [(Delegation::HandOff, true), (Delegation::ToolLoop, false)] {
            let chat = ScriptedChat::delayed(
                Duration::from_millis(5),
                vec![ChatDelta::Text("Ready.".into()), ChatDelta::End],
            );
            let tts = ScriptedTts::default();
            let mut limits = if agent {
                RunLimits::AGENT
            } else {
                RunLimits::MODEL
            };
            limits.ack_after = agent.then_some(Duration::from_millis(3));
            let behaviour = behaviour(limits, delegation);
            let (sink, mut source) = reply_channel(2);
            let events = TestEvents::default();
            let observer = TestObserver::default();
            let mut request = request(
                "Question", &chat, &tts, &NoTools, &behaviour, &sink, &observer,
            );
            request.ack = Some(Path::new("ack.mp3"));
            let started = Instant::now();
            let run = reply(request, &events);
            let consume = async {
                let mut seen = Vec::new();
                for _ in 0..if agent { 2 } else { 1 } {
                    let item = source.recv().await.unwrap();
                    seen.push((item.kind, started.elapsed()));
                    source.started(item.kind);
                }
                seen
            };
            let (outcome, seen) = tokio::join!(run, consume);
            outcome.unwrap();
            if agent {
                assert_eq!(
                    seen,
                    [
                        (AudioKind::Ack, Duration::from_millis(3)),
                        (AudioKind::Sentence, Duration::from_millis(5)),
                    ]
                );
                assert_eq!(*observer.guards.lock().unwrap(), [Guard::Ack]);
            } else {
                assert_eq!(seen[0].0, AudioKind::Sentence);
                assert!(observer.guards.lock().unwrap().is_empty());
            }
        }
    }
}
