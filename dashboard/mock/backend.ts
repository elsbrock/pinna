// A made-up satellite for `npm run dev:mock`: DeviceService and AssistantService
// answered from memory, so the dashboard shows data with no satellite or
// virtual Echo Dot. Reads and the settings a page changes work and push a new
// Status; what needs real hardware answers that the mock doesn't do it. Every
// method of both services is served, found from the generated service
// descriptors, so a new RPC never breaks the mock.
import { create } from "@bufbuild/protobuf";
import type { GenService, GenServiceMethods } from "@bufbuild/protobuf/codegenv2";
import { Code, ConnectError, type HandlerContext, type ServiceImpl } from "@connectrpc/connect";

import { RING_PIXELS } from "../../design/ring.ts";
import {
  type Assistant,
  AssistantSchema,
  AssistantService,
  AssistantSlot,
  ChoiceKind,
  Delegation,
  History,
  ProviderConnectionSchema,
  Reasoning,
  type SatelliteGroupView,
  SlotChoiceSchema,
} from "../src/gen/pinna/v1/assistant_pb.ts";
import {
  AirPlayState,
  BluetoothState,
  DeviceService,
  InstallUpdates,
  JoinStage,
  LastBootOutcome,
  MediaState,
  Mode,
  Phase,
  ReleaseAction,
  SendspinState,
  type Settings,
  type Status,
  StatusSchema,
  UpdateChannel,
  type UpdateSettingsRequest,
  WifiSecurity,
} from "../src/gen/pinna/v1/device_pb.ts";

const MESSAGE_UNSUPPORTED = "The mock backend doesn't do this. Use the virtual Echo Dot (just dev-vm).";

/** Methods that need a real satellite: they answer Unimplemented. */
const UNSUPPORTED = new Set([
  "reboot",
  "factoryReset",
  "installUpdate",
  "setActiveSlot",
  "joinWifi",
  "keepWifi",
  "forgetWifi",
  "recordAudio",
  "stopRecording",
  "startCalibration",
  "stopCalibration",
  "addWakeWord",
  "removeWakeWord",
  "startBluetoothPairing",
  "stopBluetoothPairing",
  "forgetBluetoothDevice",
  "startConfirmation",
  "watchConfirmation",
  "cancelConfirmation",
  "setDashboardPassword",
  "removeDashboardPassword",
  "signOutEverywhere",
  "createToken",
  "openHomeAssistantWindow",
  "allowGroupJoin",
  "watchInstall",
  "setSatelliteGroup",
  "addGroupMember",
  "removeGroupMember",
  "playSample",
  "askText",
]);

const unsupported = () => new ConnectError(MESSAGE_UNSUPPORTED, Code.Unimplemented);

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const started = Date.now() - (3 * HOUR + 12 * MINUTE);

const settings: Settings = {
  $typeName: "pinna.v1.Settings",
  wakeWord: true,
  wakeSensitivity: 60,
  ledBrightness: 70,
  wakeWords: ["alexa_v0.1"],
  friendlyName: "Kitchen",
  defaultName: false,
  wakeSound: true,
  url: "",
  airplay: false,
  bluetooth: false,
  timeZone: "Europe/Berlin",
  replyVolume: undefined,
  listeningDirection: true,
  updateChannel: UpdateChannel.BETA,
  mode: Mode.HOME_ASSISTANT,
  groupMessages: true,
  sendspin: false,
  installUpdates: InstallUpdates.MANUALLY,
  installWindowStart: "03:00",
  installWindowEnd: "05:00",
  includeInStatistics: true,
};

const state = {
  volume: 40,
  softwareMuted: false,
  phase: Phase.IDLE,
  notification: false,
  ledTest: false,
  ringLight: { on: false, brightness: 60, rgb: 0x2f81f7, effect: "Solid", effects: ["Solid", "Pulse", "Flash"] },
  lastCheck: Date.now() - 20 * MINUTE,
  checking: false,
};

const listeners = new Set<() => void>();
/** Tell every WatchStatus stream that the state changed. */
const changed = () => listeners.forEach((wake) => wake());

function status(): Status {
  return create(StatusSchema, {
    version: "0.6.0-mock",
    name: "pinna-6d484b",
    mac: "a0:d0:5b:6d:48:4b",
    uptimeSeconds: BigInt(Math.floor((Date.now() - started) / 1000)),
    phase: state.phase,
    muted: state.softwareMuted,
    hardwareMuted: false,
    softwareMuted: state.softwareMuted,
    volume: state.volume,
    haConnected: true,
    voiceSubscribed: true,
    wakeWord: {
      phrase: "Alexa",
      enabled: settings.wakeWord && !state.softwareMuted,
      threshold: 0.55,
      recent: [
        { unixMs: BigInt(Date.now() - 42 * MINUTE), woke: true, score: 0.91, wakesFrom: 20, phrase: "Alexa" },
        { unixMs: BigInt(Date.now() - 17 * MINUTE), woke: false, score: 0.34, wakesFrom: 75, reachedThreshold: false },
        { unixMs: BigInt(Date.now() - 6 * MINUTE), woke: true, score: 0.87, wakesFrom: 28, phrase: "Alexa" },
      ],
      available: [
        { id: "alexa_v0.1", phrase: "Alexa" },
        { id: "hey_jarvis_v0.1", phrase: "Hey Jarvis" },
        { id: "okay_nabu_v0.1", phrase: "Okay Nabu" },
      ],
    },
    timers: [],
    audioBackend: "echodot",
    ledBackend: "is31",
    settings,
    model: "echo-dot-2",
    notification: state.notification,
    virtualDevice: false,
    lastBoot: { outcome: LastBootOutcome.RESTARTED, reason: "update", summary: "restarted (update)" },
    apiEncrypted: true,
    apiKeyFromImage: false,
    ledTest: state.ledTest,
    airplay: { state: settings.airplay ? AirPlayState.AIRPLAY_STATE_READY : AirPlayState.AIRPLAY_STATE_OFF, name: settings.airplay ? "Kitchen" : "" },
    bluetooth: { state: settings.bluetooth ? BluetoothState.READY : BluetoothState.OFF },
    ringLight: state.ringLight,
    media: { state: MediaState.IDLE, available: true, volume: state.volume },
    mode: settings.mode,
    assistant: {
      ready: true,
      sttReady: true,
      brainReady: true,
      ttsReady: true,
      label: "openai: gpt-4o-mini",
    },
    heldMessages: 0,
    sendspin: { state: settings.sendspin ? SendspinState.READY : SendspinState.OFF, name: settings.sendspin ? "Kitchen" : "" },
  });
}

const reply = () => ({ status: status() });

/** Apply the fields a request sets; an unset optional field leaves the setting alone. */
function applySettings(req: UpdateSettingsRequest) {
  const { wakeWords, replyFollowsVolume, hostname: _hostname, ...fields } = req;
  for (const [key, value] of Object.entries(fields)) {
    if (key === "$typeName" || value === undefined) continue;
    (settings as Record<string, unknown>)[key] = value;
  }
  if (wakeWords) settings.wakeWords = wakeWords.ids;
  if (replyFollowsVolume) settings.replyVolume = undefined;
  if (req.friendlyName !== undefined) settings.defaultName = false;
}

/** Resolve after `ms`, on the next change, or when the call ends. */
function nextChange(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const done = () => {
      clearTimeout(timer);
      listeners.delete(done);
      signal.removeEventListener("abort", done);
      resolve();
    };
    const timer = setTimeout(done, ms);
    listeners.add(done);
    signal.addEventListener("abort", done);
  });
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const done = () => {
      clearTimeout(timer);
      signal.removeEventListener("abort", done);
      resolve();
    };
    const timer = setTimeout(done, ms);
    signal.addEventListener("abort", done);
  });
}

// What pinnad logs, in its wording (crates/pinnad/src).
const BACKLOG: [string, string, string][] = [
  ["INFO", "pinnad", "pinnad 0.6.0-mock starting"],
  ["INFO", "pinnad::config", "local configuration: /etc/pinnad.toml"],
  ["WARN", "pinnad::config", "/var/lib/pinna/pinnad.toml not found, using defaults"],
  ["INFO", "pinnad::network", "wlan0 joined Home"],
  ["INFO", "pinnad::clock", "the clock synced"],
  ["INFO", "pinnad::settings", "time zone: Europe/Berlin"],
  ["INFO", "pinnad::api", "ESPHome API listening on 0.0.0.0:6053"],
  ["INFO", "pinnad::web", "dashboard listening on 0.0.0.0:80"],
  ["INFO", "pinnad::api", "Home Assistant connected"],
  ["INFO", "pinnad::voice", "voice assistant subscribed"],
  ["INFO", "pinnad::updates", "updates: checking beta"],
  ["INFO", "pinnad::updates", "update check: no new release"],
  ["INFO", "pinnad::voice", "wake word \"Alexa\" detected (score 0.91)"],
  ["INFO", "pinnad::voice", "run finished"],
  ["INFO", "pinnad::settings", "wake sound: true"],
  ["WARN", "pinnad::wifi", "wlan0: signal -71 dBm, below -70"],
  ["INFO", "pinnad::voice", "wake word \"Alexa\" detected (score 0.87)"],
  ["INFO", "pinnad::voice", "timer alarm dismissed by wake word"],
  ["INFO", "pinnad::settings", "renamed to \"Kitchen\""],
  ["WARN", "pinnad::media", "media: http://radio.example/stream: connection reset"],
  ["ERROR", "pinnad::media", "media: http://radio.example/stream: the server closed the stream"],
  ["INFO", "pinnad::updates", "updates: checking beta"],
  ["INFO", "pinnad::updates", "update check: no new release"],
  ["INFO", "pinnad::api", "Home Assistant connected"],
  ["WARN", "pinnad::api", "the ESPHome connection from 192.168.1.20 ended: timeout"],
  ["INFO", "pinnad::api", "Home Assistant connected"],
  ["INFO", "pinnad::settings", "install updates: Manually"],
  ["INFO", "pinnad::voice", "hardware mute: false"],
  ["INFO", "pinnad::voice", "wake word \"Alexa\" detected (score 0.79)"],
  ["INFO", "pinnad::voice", "run finished"],
  ["INFO", "pinnad::leds", "ring: idle"],
  ["INFO", "pinnad::settings", "group messages: true"],
  ["INFO", "pinnad::updates", "updates: checking beta"],
  ["INFO", "pinnad::updates", "update check: no new release"],
  ["INFO", "pinnad::voice", "wake word \"Alexa\" detected (score 0.93)"],
  ["INFO", "pinnad::voice", "run finished"],
  ["INFO", "pinnad::api", "Home Assistant connected"],
  ["INFO", "pinnad::settings", "install updates: Manually"],
  ["INFO", "pinnad::updates", "update check: no new release"],
  ["INFO", "pinnad::voice", "wake word \"Alexa\" detected (score 0.85)"],
];

const AGAIN: [string, string, string][] = [
  ["INFO", "pinnad::api", "Home Assistant connected"],
  ["INFO", "pinnad::voice", "run finished"],
  ["INFO", "pinnad::updates", "update check: no new release"],
];

const timeZones = [
  "UTC",
  "Europe/Berlin",
  "Europe/London",
  "Europe/Paris",
  "Europe/Madrid",
  "Europe/Helsinki",
  "America/New_York",
  "America/Chicago",
  "America/Denver",
  "America/Los_Angeles",
  "Asia/Tokyo",
  "Australia/Sydney",
];

const wifiNetworks = [
  { ssid: "Home", signalDbm: -52, frequencyMhz: 5180, security: WifiSecurity.WPA_PSK, saved: true, current: true },
  { ssid: "Home-2.4", signalDbm: -61, frequencyMhz: 2437, security: WifiSecurity.WPA_PSK, saved: true, current: false },
  { ssid: "FRITZ!Box 7590 QR", signalDbm: -74, frequencyMhz: 2412, security: WifiSecurity.WPA_PSK, saved: false, current: false },
  { ssid: "Neighbour", signalDbm: -83, frequencyMhz: 5240, security: WifiSecurity.SAE, saved: false, current: false },
  { ssid: "Guest", signalDbm: -88, frequencyMhz: 2462, security: WifiSecurity.OPEN, saved: false, current: false },
];

const device: Partial<ServiceImpl<typeof DeviceService>> = {
  getStatus: reply,
  setVolume: (req) => {
    state.volume = Math.min(100, req.percent);
    changed();
    return reply();
  },
  setStreamVolume: reply,
  setStreamMetadata: reply,
  setMute: (req) => {
    state.softwareMuted = req.muted;
    changed();
    return reply();
  },
  startConversation: reply,
  stopConversation: reply,
  clearNotification: () => {
    state.notification = false;
    changed();
    return reply();
  },
  ledTest: () => {
    state.ledTest = true;
    changed();
    // The real pattern runs four seconds: one for each of four phases.
    setTimeout(() => {
      state.ledTest = false;
      changed();
    }, 4_000);
    return {};
  },
  async *watchStatus(_req, ctx: HandlerContext) {
    while (!ctx.signal.aborted) {
      yield reply();
      // Uptime changes without a state change: send again every 10 s.
      await nextChange(10_000, ctx.signal);
    }
  },
  async *watchRing(_req, ctx: HandlerContext) {
    const frame = new Uint8Array(RING_PIXELS * 3);
    yield { frame };
    await sleep(2 ** 31 - 1, ctx.signal);
  },
  updateSettings: (req) => {
    applySettings(req);
    changed();
    return reply();
  },
  setRingLight: (req) => {
    const { on, brightness, rgb, effect } = req;
    state.ringLight = {
      ...state.ringLight,
      ...(on !== undefined && { on }),
      ...(brightness !== undefined && { brightness }),
      ...(rgb !== undefined && { rgb }),
      ...(effect !== undefined && { effect }),
    };
    changed();
    return reply();
  },
  controlMedia: reply,
  playMedia: reply,
  listTimeZones: () => ({ zones: timeZones }),
  listSatellites: () => ({ satellites: [], onNetwork: true, searched: true }),
  getNetwork: () => ({
    hostname: "pinna-6d484b",
    mdnsHostname: "pinna-6d484b",
    interfaces: [
      {
        name: "wlan0",
        mac: "a0:d0:5b:6d:48:4b",
        up: true,
        wireless: true,
        addresses: ["192.168.1.42/24", "fd12:3456:789a::42/64", "fe80::a2d0:5bff:fe6d:484b/64"],
        wifi: { ssid: "Home", signalDbm: -52, frequencyMhz: 5180, txBitrate: "433.3 MBit/s" },
      },
    ],
  }),
  scanWifi: () => ({ networks: wifiNetworks }),
  listWifi: () => ({
    saved: [
      { ssid: "Home", security: WifiSecurity.WPA_PSK, hidden: false, current: true },
      { ssid: "Home-2.4", security: WifiSecurity.WPA_PSK, hidden: false, current: false },
    ],
    joining: false,
    lastJoin: { ssid: "Home", stage: JoinStage.JOINED, address: "192.168.1.42" },
  }),
  async *watchLogs(_req, ctx: HandlerContext) {
    const now = Date.now();
    for (const [i, [level, target, message]] of BACKLOG.entries()) {
      yield { line: { unixMs: BigInt(now - (BACKLOG.length - i) * 7_000), level, target, message } };
    }
    for (let n = 0; !ctx.signal.aborted; n++) {
      await sleep(5_000, ctx.signal);
      if (ctx.signal.aborted) return;
      const [level, target, message] = AGAIN[n % AGAIN.length];
      yield { line: { unixMs: BigInt(Date.now()), level, target, message } };
    }
  },
  getUpdates: () => ({
    available: true,
    bootedVersion: "0.6.0-mock",
    slots: [
      { name: "a", version: "0.6.0-mock", state: "good", triesLeft: 0, active: true, booted: true },
      { name: "b", version: "0.6.0-beta.4", state: "good", triesLeft: 0, active: false, booted: false, olderThanRunning: true },
    ],
    installing: false,
    suggestedManifest: "https://updates.getpinna.dev/beta/echo-dot-2/manifest.json",
    check: {
      checking: state.checking,
      finishedUnixMs: BigInt(state.lastCheck),
      noRelease: true,
      nextUnixMs: BigInt(state.lastCheck + 6 * HOUR),
    },
    release: { version: "0.6.0-mock", channel: "beta", action: ReleaseAction.UP_TO_DATE },
  }),
  checkForUpdates: () => {
    state.lastCheck = Date.now();
    changed();
    return {};
  },
  getSecurity: () => ({ security: { passwordSet: false, leftOpen: true, offerPending: false, signedIn: false } }),
  leaveDashboardOpen: () => ({ security: { passwordSet: false, leftOpen: true } }),
};

// Assistant: a standalone setup that passed its checks, so the pages have
// something to show whichever mode is on.
const assistantView: Assistant = create(AssistantSchema, {
  ready: true,
  connections: [
    { id: "openai", preset: "openai", baseUrl: "https://api.openai.com/v1", revision: 1n, keySet: true, keyLast4: "9f3a" },
  ],
  stt: { connection: "openai", model: "whisper-1", ready: true },
  brain: { connection: "openai", model: "gpt-4o-mini", ready: true },
  tts: { connection: "openai", model: "alloy", ready: true, speechModel: "tts-1" },
  brainOverride: { delegation: Delegation.UNSPECIFIED, history: History.UNSPECIFIED, reasoning: Reasoning.UNSPECIFIED },
  language: "English",
  place: { name: "Berlin", latitude: 52.52, longitude: 13.405, timeZone: "Europe/Berlin" },
  followUps: true,
  ack: { enabled: true, phrase: "One moment." },
  stations: [{ name: "Radio Eins", url: "https://radioeins.example/stream" }],
  presets: [
    { id: "openai", baseUrl: "https://api.openai.com/v1", stt: true, brain: true, tts: true, model: "gpt-4o-mini" },
    { id: "openrouter", baseUrl: "https://openrouter.ai/api/v1", brain: true, model: "openai/gpt-4o-mini" },
    { id: "elevenlabs", baseUrl: "https://api.elevenlabs.io", stt: true, tts: true },
    { id: "ollama", baseUrl: "http://<host>:11434/v1", brain: true, thinking: true },
    { id: "custom", baseUrl: "" },
  ],
  voiceRules: "Answer in one or two short sentences, in plain words, without lists or markdown.",
  deviceTools: [
    { name: "set_volume", description: "Set the speaker volume.", brain: true },
    { name: "ring_light", description: "Switch the ring light and set its colour.", brain: true },
    { name: "set_timer", description: "Start a timer.", brain: true },
    { name: "play_station", description: "Play an internet radio station.", brain: true },
    { name: "mute_microphone", description: "Mute the microphones.", brain: true, offByDefault: true },
  ],
});

/** A new revision each time the view changes, as the satellite's edits do. */
function assistantReply() {
  return { assistant: assistantView };
}

const group: SatelliteGroupView = {
  $typeName: "pinna.v1.SatelliteGroupView",
  on: false,
  inGroup: false,
  control: false,
  members: [],
  candidates: [],
};

const assistant: Partial<ServiceImpl<typeof AssistantService>> = {
  getAssistant: assistantReply,
  setConnection: (req) => {
    const at = assistantView.connections.findIndex((c) => c.id === req.id);
    const next = {
      id: req.id || req.preset,
      preset: req.preset,
      baseUrl: req.baseUrl,
      revision: (assistantView.connections[at]?.revision ?? 0n) + 1n,
      keySet: req.clearKey ? false : req.key ? true : (assistantView.connections[at]?.keySet ?? false),
      keyLast4: req.key ? req.key.slice(-4) : (assistantView.connections[at]?.keyLast4 ?? ""),
    };
    if (at < 0) assistantView.connections.push(create(ProviderConnectionSchema, next));
    else assistantView.connections[at] = create(ProviderConnectionSchema, next);
    return assistantReply();
  },
  deleteConnection: (req) => {
    assistantView.connections = assistantView.connections.filter((c) => c.id !== req.id);
    return assistantReply();
  },
  setSlot: (req) => {
    const slot =
      req.slot === AssistantSlot.STT ? "stt" : req.slot === AssistantSlot.BRAIN ? "brain" : req.slot === AssistantSlot.TTS ? "tts" : null;
    if (slot) {
      assistantView[slot] = create(SlotChoiceSchema, {
        connection: req.connection,
        model: req.model,
        speechModel: req.speechModel,
        ready: true,
      });
    }
    return assistantReply();
  },
  updateAssistant: (req) => {
    if (req.language !== undefined) assistantView.language = req.language;
    if (req.prompt !== undefined) assistantView.prompt = req.prompt;
    if (req.defaultPrompt) assistantView.prompt = undefined;
    if (req.followUps !== undefined) assistantView.followUps = req.followUps;
    if (req.ack) assistantView.ack = req.ack;
    if (req.tools) assistantView.tools = { ...assistantView.tools, ...req.tools.on };
    if (req.stations) assistantView.stations = req.stations.stations;
    return assistantReply();
  },
  listChoices: (req) => ({
    choices:
      req.kind === ChoiceKind.VOICE
        ? [
            { id: "alloy", label: "Alloy" },
            { id: "nova", label: "Nova" },
            { id: "onyx", label: "Onyx" },
          ]
        : [
            { id: "gpt-4o-mini", label: "gpt-4o-mini" },
            { id: "gpt-4o", label: "gpt-4o" },
            { id: "whisper-1", label: "whisper-1" },
          ],
  }),
  getSatelliteGroup: () => ({ group }),
};

/** Serve every method of `service`: the table's handler, else Unimplemented, else an empty answer. */
function implement<S extends GenService<GenServiceMethods>>(
  service: S,
  table: Partial<ServiceImpl<S>>,
): ServiceImpl<S> {
  const impl: Record<string, unknown> = {};
  for (const method of service.methods) {
    const known = (table as Record<string, unknown>)[method.localName];
    if (UNSUPPORTED.has(method.localName)) {
      impl[method.localName] =
        method.methodKind === "unary"
          ? () => {
              throw unsupported();
            }
          : async function* () {
              throw unsupported();
            };
    } else if (known) impl[method.localName] = known;
    else if (method.methodKind === "unary") impl[method.localName] = () => create(method.output);
    else impl[method.localName] = async function* () {};
  }
  return impl as ServiceImpl<S>;
}

export const deviceImpl = implement(DeviceService, device);
export const assistantImpl = implement(AssistantService, assistant);
