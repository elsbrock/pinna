//! What the hardware is: the Echo models Pinna runs on, how to tell them
//! apart, and the facts about each board that the rest of Pinna needs
//! (ADR 0032). Data and detection only, with the standard library alone, so
//! pinna-init, the updater and the host tools can use it as well as pinnad.
//! The drivers that act on these facts are in `pinna-hal`.
//!
//! A board is described once, here. Code that drives the hardware takes its
//! paths, key codes, PCM names and tuning from the [`Board`] it runs on,
//! instead of matching on the model itself.

use std::path::Path;

pub mod detect;

pub use detect::{
    DT_COMPATIBLE, DT_IDME_DIR, IDME_DIR, detect, detect_in, device_type_id, storage,
};

/// An Echo model Pinna runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Model {
    /// Echo Dot (2nd gen), `biscuit`.
    EchoDot2,
    /// Echo (2nd gen), `radar`.
    Echo2,
}

impl Model {
    pub const ALL: [Model; 2] = [Model::EchoDot2, Model::Echo2];

    /// The stable id: the dashboard draws the device by it, a release
    /// manifest names the models it is for by it, and the control API and
    /// the metrics report it.
    pub const fn id(self) -> &'static str {
        match self {
            Model::EchoDot2 => "echo-dot-2",
            Model::Echo2 => "echo-2",
        }
    }

    pub fn from_id(id: &str) -> Option<Model> {
        Model::ALL.into_iter().find(|m| m.id() == id)
    }

    /// The name owners know the device by, as the docs write it.
    pub const fn name(self) -> &'static str {
        match self {
            Model::EchoDot2 => "Echo Dot (2nd gen)",
            Model::Echo2 => "Echo (2nd gen)",
        }
    }

    /// The model Home Assistant shows on the device page. Its capitals differ
    /// from [`Model::name`]; Home Assistant keeps what it was first told.
    pub const fn home_assistant_name(self) -> &'static str {
        match self {
            Model::EchoDot2 => "Echo Dot (2nd Gen)",
            Model::Echo2 => "Echo (2nd Gen)",
        }
    }

    /// The model without its generation, for the name a satellite gets
    /// before the owner chooses one.
    pub const fn short_name(self) -> &'static str {
        match self {
            Model::EchoDot2 => "Echo Dot",
            Model::Echo2 => "Echo",
        }
    }

    pub const fn board(self) -> &'static Board {
        match self {
            Model::EchoDot2 => &ECHO_DOT_2,
            Model::Echo2 => &ECHO_2,
        }
    }
}

/// One board: everything Pinna's code needs to know to drive it.
#[derive(Debug, PartialEq)]
pub struct Board {
    pub model: Model,
    /// The bootloader's idme `device_type_id`.
    pub device_type_id: &'static str,
    /// The product fastboot reports (`fastboot getvar product`).
    pub fastboot_product: &'static str,
    pub soc: Soc,
    /// Where the partitions live, and so how the initramfs finds them.
    pub storage: StorageKind,
    pub ring: Ring,
    /// Key codes the board's input devices send, and the button each is.
    pub keys: &'static [(u16, Button)],
    pub privacy: Privacy,
    pub audio: Audio,
    pub mics: Mics,
    pub wifi: Wifi,
}

/// The system on chip. Every board of one SoC boots the same kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Soc {
    /// MediaTek MT8163, 4x Cortex-A53 run in AArch32.
    Mt8163,
}

impl Soc {
    /// The SoC's entry in the device tree's root `compatible` list.
    pub const fn compatible(self) -> &'static str {
        match self {
            Soc::Mt8163 => "mediatek,mt8163",
        }
    }
}

/// Where a board keeps its partitions, and so how the initramfs finds them.
/// The logical names are the same either way (`misc`, `boot_a`, `boot_b`,
/// `system_a`, `system_b`, `userdata`); only how a name resolves to a device
/// differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageKind {
    /// A GPT partition table on eMMC: a name is a partition's GPT `PARTNAME`,
    /// and its device node is a block device. The MT8163 and MT8516 Echos.
    Gpt,
    /// UBI volumes on raw NAND: a name is a UBI volume's name, and its device
    /// node is the volume's character device. The MT8519 Echos (ADR 0034).
    Ubi,
}

/// The light ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ring {
    pub driver: RingDriver,
    /// LEDs on the ring. Pinna draws 12 logical pixels; a driver for a
    /// board with fewer LEDs folds them.
    pub leds: usize,
    /// The two pixels at the front centre of the ring.
    pub front_centre: [usize; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingDriver {
    /// ISSI IS31FL3236 behind the kernel driver's `frame` and
    /// `boot_animation` attributes (kernel patch 0011), found under
    /// `/sys/bus/i2c/devices`.
    Is31fl3236Frame,
}

impl RingDriver {
    /// The driver as a log line names it.
    pub const fn name(self) -> &'static str {
        match self {
            RingDriver::Is31fl3236Frame => "IS31FL3236 via sysfs",
        }
    }
}

/// A physical button, by what Pinna does with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    VolumeUp,
    VolumeDown,
    Action,
    Mute,
}

// Linux input event codes (linux/input-event-codes.h).
pub const KEY_MUTE: u16 = 113;
pub const KEY_VOLUMEDOWN: u16 = 114;
pub const KEY_VOLUMEUP: u16 = 115;
pub const KEY_POWER: u16 = 116;
pub const KEY_HELP: u16 = 138;
pub const KEY_MICMUTE: u16 = 248;

/// The MT8163 Echos' keys. KEY_POWER is the microphone button there
/// (measured on LibreEcho's 6.1 kernel, whose device tree Pinna's is ported
/// from: ten mute presses gave ten KEY_POWER events; there is no separate
/// power button), so it is `Mute`, not `Action`. KEY_MUTE and KEY_MICMUTE
/// are taken as `Mute` too.
pub const MT8163_KEYS: &[(u16, Button)] = &[
    (KEY_VOLUMEUP, Button::VolumeUp),
    (KEY_VOLUMEDOWN, Button::VolumeDown),
    (KEY_MUTE, Button::Mute),
    (KEY_MICMUTE, Button::Mute),
    (KEY_POWER, Button::Mute),
    (KEY_HELP, Button::Action),
];

impl Button {
    /// The button a key code is on the MT8163 Echos, and on the virtual
    /// Echo Dot, which sends the same codes.
    pub fn from_code(code: u16) -> Option<Button> {
        button_in(MT8163_KEYS, code)
    }
}

/// The button `code` is in `keys`.
pub fn button_in(keys: &[(u16, Button)], code: u16) -> Option<Button> {
    keys.iter().find(|(k, _)| *k == code).map(|(_, b)| *b)
}

/// The hardware microphone switch. Pressing the button cuts the
/// microphones in hardware; the kernel reports the latch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Privacy {
    /// Where the latch reads `0` or `1`, first readable wins.
    pub state: &'static [&'static str],
    /// Where writing `1` closes the latch from software. Only the button
    /// opens it again.
    pub trigger: &'static [&'static str],
    /// The red lamp the latch lights, when the kernel exposes it.
    pub mute_lamp: &'static str,
}

/// Kernel patch 0012 (`amz_privacy`); older kernels name the device
/// `amz-privacy`.
pub const AMZ_PRIVACY: Privacy = Privacy {
    state: &[
        "/sys/devices/platform/amz_privacy/privacy_state",
        "/sys/devices/platform/amz-privacy/privacy_state",
    ],
    trigger: &[
        "/sys/devices/platform/amz_privacy/privacy_trigger",
        "/sys/devices/platform/amz-privacy/privacy_trigger",
    ],
    mute_lamp: "/sys/devices/platform/amz_privacy/mute_lamp",
};

/// The sound card.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Audio {
    /// The card's id in `/proc/asound/cards`.
    pub card: &'static str,
    /// The speaker's PCM.
    pub playback: Pcm,
    /// The microphones' PCM.
    pub capture: Pcm,
    /// Which mixer sequences set the card up (`pinna-audio`'s `mixer`).
    pub codec: Codec,
    /// The speaker's tuning, unless a developer picks another.
    pub speaker: SpeakerTuning,
    /// The playback channels the speaker reproduces.
    pub speaker_channels: SpeakerChannels,
}

/// A channel of the stereo playback PCM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Left,
    Right,
}

/// Which playback channels reach the speaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeakerChannels {
    Both,
    /// One channel only; the other never sounds. pinnad plays a mono mix, so
    /// nothing is lost and both channels carry what the speaker plays.
    Only(Channel),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pcm {
    /// The ALSA device, `hw:<card>,<device>`.
    pub device: &'static str,
    pub rate: u32,
    pub channels: usize,
    pub format: SampleFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    S16Le,
    /// 24 bits in 3 bytes, little endian.
    S24_3Le,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// The MT8163 Echos: a TLV320AIC32x4 for the speaker, TLV320ADC3101s
    /// for the microphones behind the SPI FPGA, the amplifier behind
    /// `Ext_Speaker_Amp_Switch` (kernel patches 0014 to 0016). The sequences
    /// are LibreEcho's (micd.c, audio_engine.c).
    Mt8163Aic,
}

/// How the speaker is tuned: the codec's profile (a mixer control, kernel
/// patch 0014) and the gain Pinna adds in software.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeakerTuning {
    pub codec_profile: CodecProfile,
    /// Gain before the volume curve, which tops out at -12 dB.
    pub pre_gain_db: f32,
    /// The stock firmware's extra gain above 70 % volume; the speaker
    /// chain's limiter keeps it under full scale.
    pub volume_boost: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecProfile {
    /// The Echo Dot (2nd gen)'s single speaker.
    Flat,
    /// The Echo (2nd gen)'s woofer and tweeter.
    Radar,
}

impl CodecProfile {
    /// The item of the "Speaker Codec Profile" control.
    pub const fn control_item(self) -> &'static str {
        match self {
            CodecProfile::Flat => "Flat",
            CodecProfile::Radar => "Radar",
        }
    }
}

impl SpeakerTuning {
    /// The Echo Dot (2nd gen): +6 dB, and the boost above 70 %.
    pub const FLAT: SpeakerTuning = SpeakerTuning {
        codec_profile: CodecProfile::Flat,
        pre_gain_db: 6.0,
        volume_boost: true,
    };
    /// The Echo (2nd gen): the codec's own profile, no extra gain.
    pub const RADAR: SpeakerTuning = SpeakerTuning {
        codec_profile: CodecProfile::Radar,
        pre_gain_db: 0.0,
        volume_boost: false,
    };

    /// The tuning a developer names in `pinnad.toml` (`speaker_profile`):
    /// `Flat` in any case, otherwise `Radar`.
    pub fn named(name: &str) -> SpeakerTuning {
        if name.eq_ignore_ascii_case("flat") {
            SpeakerTuning::FLAT
        } else {
            SpeakerTuning::RADAR
        }
    }
}

/// The microphone array, as the capture PCM delivers it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mics {
    /// Microphones, on the capture PCM's first lanes.
    pub count: usize,
    /// The loopback lanes after the microphones: the playback channel each
    /// carries, in order. They come in the microphones' frames, so the echo
    /// canceller's reference needs no timing of its own.
    pub loopback: &'static [Channel],
    pub geometry: MicGeometry,
    /// The lanes the stock firmware uses for speech, as a bit mask. The
    /// beamformer that points the ring at the talker listens to these.
    pub speech_mask: u8,
}

impl Mics {
    /// Every microphone, as a lane mask.
    pub const fn all(&self) -> u8 {
        ((1u16 << self.count) - 1) as u8
    }

    /// Lanes on the capture PCM: the microphones, then the loopback lanes.
    pub const fn lanes(&self) -> usize {
        self.count + self.loopback.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MicGeometry {
    /// Microphones evenly spaced on a circle, lane 0 at `first_deg` and each
    /// next lane `step_deg` further, and one in the centre at lane `centre`.
    RingAndCentre {
        radius_mm: f64,
        first_deg: f64,
        step_deg: f64,
        centre: usize,
    },
}

impl MicGeometry {
    /// Where lane `lane`'s microphone sits, in metres from the centre.
    pub fn position(&self, lane: usize) -> (f64, f64) {
        match *self {
            MicGeometry::RingAndCentre {
                radius_mm,
                first_deg,
                step_deg,
                centre,
            } => {
                if lane == centre {
                    return (0.0, 0.0);
                }
                let angle = (first_deg + lane as f64 * step_deg).to_radians();
                let r = radius_mm * 1e-3;
                (r * angle.cos(), r * angle.sin())
            }
        }
    }
}

/// The Wi-Fi and Bluetooth chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wifi {
    /// The MT8163's CONSYS, through the vendor driver (kernel patches 0018
    /// to 0031), started by `pinna-wifi.service`.
    Mt8163Consys,
}

const MT8163_AUDIO: Audio = Audio {
    card: "mt-snd-card",
    playback: Pcm {
        device: "hw:0,23",
        rate: 48_000,
        channels: 2,
        format: SampleFormat::S16Le,
    },
    capture: Pcm {
        device: "hw:0,24",
        rate: 16_000,
        channels: 9,
        format: SampleFormat::S24_3Le,
    },
    codec: Codec::Mt8163Aic,
    speaker: SpeakerTuning::FLAT,
    // The codec's right headphone driver (HPR, from R_DAC) drives the speaker
    // amplifier. On stereo music the speaker's echo follows loopback lane 8:
    // the best linear filter from lane 8 removes 12.5 dB (Echo Dot (2nd gen),
    // volume 55) and 13.3 dB (Echo (2nd gen), volume 75), from lane 7 0.8
    // and 0.9 dB. Recordings of 2026-11-21, measured with micbench (ADR 0004).
    speaker_channels: SpeakerChannels::Only(Channel::Right),
};

/// The seven-microphone array of both MT8163 Echos (LibreEcho's
/// mic_array_geometry.h): six on a 36 mm circle, one in the centre, then
/// the two loopback lanes.
const MT8163_MICS: Mics = Mics {
    count: 7,
    // The playback's left and right channel after pinnad's volume, every third
    // 48 kHz sample, bit-exact zero in silence (both models, 2026-11-21). TECHO5
    // and EchoMuse map lanes 7 and 8 the same way.
    loopback: &[Channel::Left, Channel::Right],
    geometry: MicGeometry::RingAndCentre {
        radius_mm: 36.0,
        first_deg: 150.0,
        step_deg: 60.0,
        centre: 6,
    },
    // Rear-left, rear-right, front-right, front-left.
    speech_mask: (1 << 0) | (1 << 1) | (1 << 3) | (1 << 4),
};

const MT8163_RING: Ring = Ring {
    driver: RingDriver::Is31fl3236Frame,
    leds: 12,
    front_centre: [10, 11],
};

pub static ECHO_DOT_2: Board = Board {
    model: Model::EchoDot2,
    device_type_id: "A3S5BH2HU6VAYF",
    fastboot_product: "BISCUIT",
    soc: Soc::Mt8163,
    storage: StorageKind::Gpt,
    ring: MT8163_RING,
    keys: MT8163_KEYS,
    privacy: AMZ_PRIVACY,
    audio: MT8163_AUDIO,
    mics: MT8163_MICS,
    wifi: Wifi::Mt8163Consys,
};

/// The Echo (2nd gen) boots the Echo Dot's kernel and device tree
/// (`mt8163-radar-puffin.dtb`); only its speaker differs.
pub static ECHO_2: Board = Board {
    model: Model::Echo2,
    device_type_id: "A7WXQPH584YP",
    fastboot_product: "RADAR",
    soc: Soc::Mt8163,
    storage: StorageKind::Gpt,
    ring: MT8163_RING,
    keys: MT8163_KEYS,
    privacy: AMZ_PRIVACY,
    audio: Audio {
        speaker: SpeakerTuning::RADAR,
        ..MT8163_AUDIO
    },
    mics: MT8163_MICS,
    wifi: Wifi::Mt8163Consys,
};

impl Board {
    /// Whether `path`'s device tree `compatible` list names this board's SoC.
    pub fn soc_in(&self, compatible: &Path) -> bool {
        detect::compatible_has(compatible, self.soc.compatible())
    }

    /// The capture lane that carries what the speaker plays, for the echo
    /// canceller's reference (ADR 0004): the loopback lane of the channel the
    /// speaker reproduces. `None` when it plays both channels, or when no
    /// loopback lane carries the one it plays.
    pub fn speaker_lane(&self) -> Option<usize> {
        let SpeakerChannels::Only(channel) = self.audio.speaker_channels else {
            return None;
        };
        self.mics
            .loopback
            .iter()
            .position(|&c| c == channel)
            .map(|i| self.mics.count + i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_stay_what_the_dashboard_and_manifests_use() {
        for m in Model::ALL {
            assert_eq!(Model::from_id(m.id()), Some(m));
            assert_eq!(m.board().model, m);
        }
        assert_eq!(Model::EchoDot2.id(), "echo-dot-2");
        assert_eq!(Model::Echo2.id(), "echo-2");
        assert_eq!(Model::from_id(""), None);
        assert_eq!(Model::from_id("echo-dot-3"), None);
    }

    #[test]
    fn names() {
        assert_eq!(Model::EchoDot2.name(), "Echo Dot (2nd gen)");
        assert_eq!(Model::Echo2.name(), "Echo (2nd gen)");
        assert_eq!(Model::EchoDot2.home_assistant_name(), "Echo Dot (2nd Gen)");
        assert_eq!(Model::Echo2.home_assistant_name(), "Echo (2nd Gen)");
        assert_eq!(Model::EchoDot2.short_name(), "Echo Dot");
        assert_eq!(Model::Echo2.short_name(), "Echo");
    }

    #[test]
    fn the_mt8163_echos_boot_from_gpt() {
        for m in Model::ALL {
            assert_eq!(m.board().storage, StorageKind::Gpt);
        }
        assert_ne!(StorageKind::Gpt, StorageKind::Ubi);
    }

    #[test]
    fn mt8163_keys() {
        assert_eq!(Button::from_code(KEY_VOLUMEUP), Some(Button::VolumeUp));
        assert_eq!(Button::from_code(KEY_VOLUMEDOWN), Some(Button::VolumeDown));
        assert_eq!(Button::from_code(KEY_HELP), Some(Button::Action));
        for code in [KEY_POWER, KEY_MUTE, KEY_MICMUTE] {
            assert_eq!(Button::from_code(code), Some(Button::Mute));
        }
        assert_eq!(Button::from_code(1), None);
    }

    #[test]
    fn speaker_tuning_by_name() {
        assert_eq!(SpeakerTuning::named("Flat"), SpeakerTuning::FLAT);
        assert_eq!(SpeakerTuning::named("flat"), SpeakerTuning::FLAT);
        assert_eq!(SpeakerTuning::named("Radar"), SpeakerTuning::RADAR);
        assert_eq!(SpeakerTuning::named("anything"), SpeakerTuning::RADAR);
        assert_eq!(ECHO_DOT_2.audio.speaker, SpeakerTuning::FLAT);
        assert_eq!(ECHO_2.audio.speaker, SpeakerTuning::RADAR);
    }

    #[test]
    fn the_two_mt8163_echos_differ_only_in_identity_and_speaker() {
        let (a, b) = (&ECHO_DOT_2, &ECHO_2);
        assert_eq!(a.soc, b.soc);
        assert_eq!(a.ring, b.ring);
        assert_eq!(a.keys, b.keys);
        assert_eq!(a.privacy, b.privacy);
        assert_eq!(a.mics, b.mics);
        assert_eq!(a.wifi, b.wifi);
        assert_eq!(
            Audio {
                speaker: SpeakerTuning::FLAT,
                ..b.audio
            },
            a.audio
        );
    }

    #[test]
    fn mic_lanes_fit_the_capture_pcm() {
        for m in Model::ALL {
            let b = m.board();
            assert_eq!(b.mics.lanes(), b.audio.capture.channels);
            assert_eq!(u32::from(b.mics.speech_mask) >> b.mics.count, 0);
            assert_eq!(b.mics.speech_mask & !b.mics.all(), 0);
        }
    }

    #[test]
    fn the_echo_reference_is_the_lane_the_speaker_plays() {
        assert_eq!(ECHO_DOT_2.speaker_lane(), Some(8));
        assert_eq!(ECHO_2.speaker_lane(), Some(8));
        assert_eq!(MT8163_MICS.all(), 0x7f);
        let stereo = Board {
            audio: Audio {
                speaker_channels: SpeakerChannels::Both,
                ..MT8163_AUDIO
            },
            ..ECHO_DOT_2
        };
        assert_eq!(stereo.speaker_lane(), None);
        let left = Board {
            audio: Audio {
                speaker_channels: SpeakerChannels::Only(Channel::Left),
                ..MT8163_AUDIO
            },
            ..ECHO_DOT_2
        };
        assert_eq!(left.speaker_lane(), Some(7));
    }

    #[test]
    fn ring_and_centre_positions() {
        let g = MT8163_MICS.geometry;
        assert_eq!(g.position(6), (0.0, 0.0));
        let (x, y) = g.position(0);
        assert!((x - 0.036 * 150f64.to_radians().cos()).abs() < 1e-12);
        assert!((y - 0.036 * 150f64.to_radians().sin()).abs() < 1e-12);
        for lane in 0..6 {
            let (x, y) = g.position(lane);
            assert!(((x * x + y * y).sqrt() - 0.036).abs() < 1e-12);
        }
    }
}
