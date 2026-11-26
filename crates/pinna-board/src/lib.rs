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
    /// Echo Dot (5th gen), model C2N6L4, `cannoli` (ADR 0035).
    EchoDot5,
}

impl Model {
    pub const ALL: [Model; 3] = [Model::EchoDot2, Model::Echo2, Model::EchoDot5];

    /// The stable id: the dashboard draws the device by it, a release
    /// manifest names the models it is for by it, and the control API and
    /// the metrics report it.
    pub const fn id(self) -> &'static str {
        match self {
            Model::EchoDot2 => "echo-dot-2",
            Model::Echo2 => "echo-2",
            Model::EchoDot5 => "echo-dot-5",
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
            Model::EchoDot5 => "Echo Dot (5th gen)",
        }
    }

    /// The model Home Assistant shows on the device page. Its capitals differ
    /// from [`Model::name`]; Home Assistant keeps what it was first told.
    pub const fn home_assistant_name(self) -> &'static str {
        match self {
            Model::EchoDot2 => "Echo Dot (2nd Gen)",
            Model::Echo2 => "Echo (2nd Gen)",
            Model::EchoDot5 => "Echo Dot (5th Gen)",
        }
    }

    /// The model without its generation, for the name a satellite gets
    /// before the owner chooses one.
    pub const fn short_name(self) -> &'static str {
        match self {
            Model::EchoDot2 | Model::EchoDot5 => "Echo Dot",
            Model::Echo2 => "Echo",
        }
    }

    pub const fn board(self) -> &'static Board {
        match self {
            Model::EchoDot2 => &ECHO_DOT_2,
            Model::Echo2 => &ECHO_2,
            Model::EchoDot5 => &ECHO_DOT_5,
        }
    }
}

/// One board: everything Pinna's code needs to know to drive it.
#[derive(Debug, PartialEq)]
pub struct Board {
    pub model: Model,
    /// The bootloader's idme `device_type_id`.
    pub device_type_id: &'static str,
    pub bootloader: Bootloader,
    pub soc: Soc,
    /// Where the partitions live, and so how the initramfs finds them.
    pub storage: StorageKind,
    /// How much a boot slot and a root slot hold.
    pub slots: Slots,
    pub ring: Ring,
    /// Key codes the board's input devices send, and the button each is.
    pub keys: &'static [(u16, Button)],
    pub privacy: Privacy,
    pub audio: Audio,
    pub mics: Mics,
    pub wifi: Wifi,
}

/// The system on chip. Every board of one SoC boots the same kernel.
/// The bootloader that starts Pinna, and what follows from it (ADR 0035,
/// Decision 9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bootloader {
    /// LK with the Kaeru payload (the MT8163 and MT8516 Echos): Android boot
    /// images, fastboot under this product name (`fastboot getvar product`),
    /// and a preloader that counts the slot record's tries.
    Kaeru { fastboot_product: &'static str },
    /// Amazon's U-Boot (the Echo Dot (5th gen)): FIT boot images, and a
    /// slot record whose tries it counts and whose spent trial slot it
    /// retires, falling back to the other slot (C2N6L4, 2026-11-26: each
    /// start lost a try before Linux ran, and a slot with none left was set
    /// to priority 0 and the good slot started). Its FITs' device trees name
    /// the board family `fit_board` at the
    /// start of their root `compatible` (`amzn,cannoli-dvt`): other Amazon
    /// boards share the SoC. `fit_configs` are the configuration names its
    /// U-Boot may boot (`bootm ${loadaddr}#${loadcfg}`); a FIT without one of
    /// them doesn't start when U-Boot asks for it.
    AmazonUboot {
        fit_board: &'static str,
        fit_configs: &'static [&'static str],
    },
}

/// The configurations Amazon's U-Boot on the Echo Dot (5th gen) may ask for.
/// It names the configuration from the board ID (its table maps
/// `0660**141002**21` to `cannoli-pvt-plastic`, `0660**140002**21` to
/// `cannoli-pvt-aluminum`, and so on down to `cannoli-hvt`), adds
/// `-recovery` for a recovery start, and defaults to `conf-1`. On a C2N6L4
/// (UART, 2026-11-26) restarts from Linux into slot A booted
/// `cannoli-pvt-plastic`, but restarts into slot B asked for
/// `cannoli-pvt-aluminum-recovery`: with a FIT that lacked it, U-Boot printed
/// "Could not find configuration node" and stopped in fastboot, three times
/// out of three, and with all of these names the same restart started the
/// kernel. Why slot B gets that name isn't known. Fire OS's FIT carries
/// exactly these 15; Pinna's boots the same kernel, device tree and
/// initramfs from each.
pub const CANNOLI_FIT_CONFIGS: &[&str] = &[
    "cannoli-pvt-plastic",
    "cannoli-pvt-aluminum",
    "cannoli-dvt-plastic",
    "cannoli-dvt-aluminum",
    "cannoli-evt-plastic",
    "cannoli-evt-aluminum",
    "cannoli-hvt",
    "conf-1",
    "cannoli-pvt-plastic-recovery",
    "cannoli-pvt-aluminum-recovery",
    "cannoli-dvt-plastic-recovery",
    "cannoli-dvt-aluminum-recovery",
    "cannoli-evt-plastic-recovery",
    "cannoli-evt-aluminum-recovery",
    "cannoli-hvt-recovery",
];

/// The format of a boot slot's image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootFormat {
    /// An Android boot image (`ANDROID!`).
    Android,
    /// A U-Boot FIT (a flattened device tree holding the kernel, the device
    /// trees and the ramdisk).
    Fit,
}

impl Bootloader {
    pub const fn boot_format(self) -> BootFormat {
        match self {
            Bootloader::Kaeru { .. } => BootFormat::Android,
            Bootloader::AmazonUboot { .. } => BootFormat::Fit,
        }
    }

    /// The product fastboot reports, on a bootloader that has fastboot.
    pub const fn fastboot_product(self) -> Option<&'static str> {
        match self {
            Bootloader::Kaeru { fastboot_product } => Some(fastboot_product),
            Bootloader::AmazonUboot { .. } => None,
        }
    }

    /// The board family a FIT's device trees must name, on a FIT bootloader.
    pub const fn fit_board(self) -> Option<&'static str> {
        match self {
            Bootloader::Kaeru { .. } => None,
            Bootloader::AmazonUboot { fit_board, .. } => Some(fit_board),
        }
    }

    /// The configuration names a FIT must carry, on a FIT bootloader (empty
    /// on the others).
    pub const fn fit_configs(self) -> &'static [&'static str] {
        match self {
            Bootloader::Kaeru { .. } => &[],
            Bootloader::AmazonUboot { fit_configs, .. } => fit_configs,
        }
    }

    /// Whether the bootloader counts the slot record's tries itself. Where it
    /// doesn't, `pinna-init` counts them. Kaeru's preloader and Amazon's
    /// U-Boot both do: counting again in `pinna-init` would halve a trial.
    pub const fn counts_tries(self) -> bool {
        matches!(
            self,
            Bootloader::Kaeru { .. } | Bootloader::AmazonUboot { .. }
        )
    }
}

/// How many bytes a boot slot and a root slot hold: the most an image
/// written there may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slots {
    pub boot: u64,
    pub root: u64,
}

impl Slots {
    /// The MT8163 Echos' GPT partitions: `boot_*` 16 MiB, `system_*` 768 MiB
    /// (ADR 0019).
    pub const MT8163: Slots = Slots {
        boot: 16 << 20,
        root: 768 << 20,
    };

    /// The slots every one of `models` can take.
    pub fn smallest(models: &[Model]) -> Option<Slots> {
        models.iter().map(|m| m.board().slots).reduce(|a, b| Slots {
            boot: a.boot.min(b.boot),
            root: a.root.min(b.root),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Soc {
    /// MediaTek MT8163, 4x Cortex-A53 run in AArch32.
    Mt8163,
    /// MediaTek MT8519, 4x Cortex-A55 run in AArch32 (ADR 0035).
    Mt8519,
}

impl Soc {
    pub const ALL: [Soc; 2] = [Soc::Mt8163, Soc::Mt8519];

    /// The SoC the device tree at `compatible` names, when Pinna knows it.
    pub fn in_compatible(compatible: &Path) -> Option<Soc> {
        Soc::ALL
            .into_iter()
            .find(|s| detect::compatible_has(compatible, s.compatible()))
    }

    /// The SoC's entry in the device tree's root `compatible` list.
    pub const fn compatible(self) -> &'static str {
        match self {
            Soc::Mt8163 => "mediatek,mt8163",
            Soc::Mt8519 => detect::MT8519_COMPATIBLE,
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
    /// node is the volume's character device. The MT8519 Echos (ADR 0035).
    Ubi,
}

/// The light ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ring {
    pub driver: RingDriver,
    /// LEDs on the ring. Pinna draws 12 logical pixels; a driver for a
    /// board with fewer LEDs folds them.
    pub leds: usize,
    /// The two LEDs, in the board's own numbering, that the logical
    /// front-centre pixels land on.
    pub front_centre: [usize; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingDriver {
    /// ISSI IS31FL3236 behind the kernel driver's `frame` and
    /// `boot_animation` attributes (kernel patch 0011), found under
    /// `/sys/bus/i2c/devices`.
    Is31fl3236Frame,
    /// ISSI IS31FL3246 behind Amazon's RGB LED class (kernel patch 0208):
    /// one class device per LED, `/sys/class/leds/is31fl3246:ledmod_<n>`,
    /// whose `color` takes `R G B`.
    Is31fl3246Class,
}

impl RingDriver {
    /// The driver as a log line names it.
    pub const fn name(self) -> &'static str {
        match self {
            RingDriver::Is31fl3236Frame => "IS31FL3236 via sysfs",
            RingDriver::Is31fl3246Class => "IS31FL3246 via the LED class",
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

/// The Echo Dot (5th gen)'s keys: `gpio-keys` for the volume keys, on Amazon's
/// CPLD, and the action button, on SoC GPIO 41 in the retail device tree the
/// FIT ships (the device tree's `linux,code`, kernel patches 0204 and 0212),
/// and `gpio-privacy`'s button event for the microphone button.
/// TECHO5 read the same four codes from the input devices of a C2N6L4 on
/// Amazon's kernel, with the same device tree (TECHO5 discussion #610,
/// 2026-11-21): 114, 115 and 138 from `gpio-keys`, 113 from the privacy
/// button.
pub const ECHO_DOT_5_KEYS: &[(u16, Button)] = &[
    (KEY_VOLUMEUP, Button::VolumeUp),
    (KEY_VOLUMEDOWN, Button::VolumeDown),
    (KEY_MUTE, Button::Mute),
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
    /// The red lamp the latch lights, on a board that has one.
    pub mute_lamp: Option<&'static str>,
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
    mute_lamp: Some("/sys/devices/platform/amz_privacy/mute_lamp"),
};

/// Amazon's `gpio-privacy` (kernel patches 0107 and 0218), the device tree
/// node `gpio-privacy`: `state` reads the latch and writing `1` to `enable`
/// pulses the line that closes it. The Echo Dot (5th gen) has no lamp of its
/// own; the ring shows the latch.
pub const GPIO_PRIVACY: Privacy = Privacy {
    state: &["/sys/devices/platform/gpio-privacy/state"],
    trigger: &["/sys/devices/platform/gpio-privacy/enable"],
    mute_lamp: None,
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
    /// The speaker chain's high-pass corner, in Hz: what the driver can
    /// reproduce. It follows the model, not the tuning a developer picks.
    pub speaker_highpass_hz: f32,
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
    /// 32 bits, little endian. The MT8519's capture carries the converter's
    /// samples left-aligned in them.
    S32Le,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// The MT8163 Echos: a TLV320AIC32x4 for the speaker, TLV320ADC3101s
    /// for the microphones behind the SPI FPGA, the amplifier behind
    /// `Ext_Speaker_Amp_Switch` (kernel patches 0014 to 0016). The sequences
    /// are LibreEcho's (micd.c, audio_engine.c).
    Mt8163Aic,
    /// The MT8519 Echos: Amazon's AFE routes DL2 to eTDM2 and a MAX98396
    /// amplifier, and eTDM1 from a TLV320ADC5140 to UL9, with the speaker's
    /// signal looped back onto UL9 for the echo canceller (kernel patches
    /// 0210 and 0211, mainline codec drivers).
    Mt8519Max98396,
}

/// How the speaker is tuned: the codec's profile (a mixer control, kernel
/// patch 0014), the volume curve, and the gain Pinna adds in software.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeakerTuning {
    /// `None` on a codec without the profile control.
    pub codec_profile: Option<CodecProfile>,
    /// How a volume of 0 to 100 % becomes a gain.
    pub volume_curve: VolumeCurve,
    /// Gain on top of the volume curve.
    pub pre_gain_db: f32,
    /// The stock firmware's extra gain above 70 % volume; the speaker
    /// chain's limiter keeps it under full scale.
    pub volume_boost: bool,
}

/// How a volume of 0 to 100 % becomes a gain. 0 % is always silent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VolumeCurve {
    /// LibreEcho's curve for the Echo Dot (2nd gen) (`audio_engine.c`):
    /// -60 dB at 1 %, -32 dB at 50 %, -12 dB at 100 %.
    LibreEcho,
    /// A stock firmware's volume steps in dB, from the first step above
    /// mute to the top. 1 to 100 % spread evenly over them, in dB between
    /// two steps.
    Steps(&'static [f32]),
}

/// Fire OS's speaker music curve on the Echo Dot (5th gen), steps 1 to 30,
/// in dB. Fire OS build 8349's `VolumeCurves.xml` gives each step an index
/// from 0 to 100, and audio_policy's default speaker curve turns an index
/// into dB between its points 1:-58, 20:-40, 60:-17 and 100:0. Read by
/// TECHO5 (MIT, `paths_cannoli.go`, `cannoliCurve`), which plays it with the
/// amplifier at its unity level, as Pinna does (Speaker Volume 10).
const FIRE_OS_ECHO_DOT_5_DB: [f32; 30] = [
    -54.21, -51.37, -44.74, -40.0, -36.55, -33.1, -28.5, -25.63, -22.18, -19.3, -17.0, -14.88,
    -13.6, -12.75, -11.48, -9.35, -7.65, -5.53, -4.68, -4.25, -3.83, -3.4, -2.98, -2.55, -2.13,
    -1.7, -1.28, -0.85, -0.43, 0.0,
];

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
        codec_profile: Some(CodecProfile::Flat),
        volume_curve: VolumeCurve::LibreEcho,
        pre_gain_db: 6.0,
        volume_boost: true,
    };
    /// The Echo (2nd gen): the codec's own profile, no extra gain.
    pub const RADAR: SpeakerTuning = SpeakerTuning {
        codec_profile: Some(CodecProfile::Radar),
        volume_curve: VolumeCurve::LibreEcho,
        pre_gain_db: 0.0,
        volume_boost: false,
    };
    /// The Echo Dot (5th gen): Fire OS's own volume curve, with no profile
    /// control and no extra gain. LibreEcho's curve, made for the Dot (2nd
    /// gen), played it 20 dB under Fire OS at half volume (C2N6L4,
    /// 2026-11-26: "long low volume range").
    pub const ECHO_DOT_5: SpeakerTuning = SpeakerTuning {
        codec_profile: None,
        volume_curve: VolumeCurve::Steps(&FIRE_OS_ECHO_DOT_5_DB),
        pre_gain_db: 0.0,
        volume_boost: false,
    };

    /// The tuning a developer names in `pinnad.toml` (`speaker_profile`):
    /// `Flat` or `Radar` in any case; `None` for any other name.
    pub fn named(name: &str) -> Option<SpeakerTuning> {
        if name.eq_ignore_ascii_case("flat") {
            Some(SpeakerTuning::FLAT)
        } else if name.eq_ignore_ascii_case("radar") {
            Some(SpeakerTuning::RADAR)
        } else {
            None
        }
    }
}

/// The microphone array, as the capture PCM delivers it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mics {
    /// Microphones, on the capture PCM's first lanes: microphone `i` is lane
    /// `i`, and the geometry places it.
    pub count: usize,
    /// The capture lane of the first loopback lane: `count` where the
    /// loopback lanes follow the microphones, further where lanes between
    /// them carry nothing (the Echo Dot (5th gen)'s converter's unwired
    /// fourth input).
    pub loopback_at: usize,
    /// The loopback lanes from `loopback_at`: the playback channel each
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

    /// Lanes on the capture PCM: the microphones, any unused lanes, then the
    /// loopback lanes.
    pub const fn lanes(&self) -> usize {
        self.loopback_at + self.loopback.len()
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
    /// Each microphone's place in millimetres, in the same frame: x toward
    /// the front, y 90 degrees clockwise from it, seen from above.
    Points(&'static [(f64, f64)]),
}

impl MicGeometry {
    /// Where microphone `mic` sits, in metres from the centre.
    pub fn position(&self, mic: usize) -> (f64, f64) {
        match *self {
            MicGeometry::RingAndCentre {
                radius_mm,
                first_deg,
                step_deg,
                centre,
            } => {
                if mic == centre {
                    return (0.0, 0.0);
                }
                let angle = (first_deg + mic as f64 * step_deg).to_radians();
                let r = radius_mm * 1e-3;
                (r * angle.cos(), r * angle.sin())
            }
            MicGeometry::Points(points) => {
                let (x, y) = points[mic];
                (x * 1e-3, y * 1e-3)
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
    /// MediaTek MT7663 on SDIO, through mainline `mt76` (`mt7663s`) and
    /// `btmtksdio`, with linux-firmware's files.
    Mt7663Sdio,
}

impl Wifi {
    /// Whether the chip joins WPA3-Personal (SAE) and PSK-SHA256 networks,
    /// which need management frame protection. The MT8163's vendor driver
    /// can do neither (ADR 0008). mt76 with wpa_supplicant should, but no
    /// join to a WPA3-only network has run on an Echo Dot (5th gen), so the
    /// MT7663 isn't claimed either: until it is measured, such a network is
    /// refused up front, as the dashboard says, not tried and lost.
    pub const fn supports_sae(self) -> bool {
        match self {
            Wifi::Mt8163Consys => false,
            Wifi::Mt7663Sdio => false,
        }
    }

    /// The chip as the image's scripts name it (`pinnactl wifi-chip`).
    pub const fn id(self) -> &'static str {
        match self {
            Wifi::Mt8163Consys => "mt8163-consys",
            Wifi::Mt7663Sdio => "mt7663-sdio",
        }
    }
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
    // The Echo Dot (2nd gen)'s single 40 mm driver.
    speaker_highpass_hz: 150.0,
};

/// The seven-microphone array of both MT8163 Echos (LibreEcho's
/// mic_array_geometry.h): six on a 36 mm circle, one in the centre, then
/// the two loopback lanes.
const MT8163_MICS: Mics = Mics {
    count: 7,
    loopback_at: 7,
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
    bootloader: Bootloader::Kaeru {
        fastboot_product: "BISCUIT",
    },
    soc: Soc::Mt8163,
    storage: StorageKind::Gpt,
    slots: Slots::MT8163,
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
    bootloader: Bootloader::Kaeru {
        fastboot_product: "RADAR",
    },
    soc: Soc::Mt8163,
    storage: StorageKind::Gpt,
    slots: Slots::MT8163,
    ring: MT8163_RING,
    keys: MT8163_KEYS,
    privacy: AMZ_PRIVACY,
    audio: Audio {
        speaker: SpeakerTuning::RADAR,
        // The woofer is meant to play down to 40 Hz: Amazon's tuning for
        // this model lifts 40-80 and 80-125 Hz by 3 to 8 dB over
        // 500-1000 Hz. Below that it adds excursion and no audible sound,
        // and the stock firmware holds everything under 70 Hz at 20:1 with
        // a limiter Pinna doesn't have. TECHO5's measurements (MIT) off a
        // unit, 2026-10-16: echod/internal/lib/asp/asp_test.go and asp.go.
        speaker_highpass_hz: 40.0,
        ..MT8163_AUDIO
    },
    mics: MT8163_MICS,
    wifi: Wifi::Mt8163Consys,
};

/// The Echo Dot (5th gen), C2N6L4 (`cannoli`): an MT8519 with SPI-NAND
/// (ADR 0035). Its audio runs without Amazon's DSP, as TECHO5 found it on a
/// C2N6L4 under Amazon's kernel, whose AFE driver Pinna ports (TECHO5
/// discussion #610, 2026-11-21 to 2026-11-23):
///
/// - The speaker plays DL2, the card's playback device 1, routed to eTDM2
///   and the MAX98396 at a fixed 96 kHz. DL2 takes 16-bit samples and eTDM2
///   widens them to its 32-bit slots.
/// - The microphones are UL9, capture device 9, at 96 kHz: the
///   TLV320ADC5140's four slots on lanes 0 to 3, then DL2 looped back on
///   lanes 4 and 5. Lanes 0 to 2 are the three microphones; lane 3, the
///   converter's unwired fourth input, stays digitally silent. The loopback
///   leads the microphones by 0.5 ms with the same polarity.
pub static ECHO_DOT_5: Board = Board {
    model: Model::EchoDot5,
    // Read from the owner's C2N6L4 (LK's idme record in the device tree,
    // Pinna 6.12 RAM boot, 2026-11-24).
    device_type_id: "A4ZXE0RM7LQ7A",
    // The product Fire OS reports on the same unit (`getprop`, 2026-11-22).
    // Stock U-Boot's fastboot product was not read: Pinna doesn't flash this
    // model through fastboot.
    bootloader: Bootloader::AmazonUboot {
        fit_board: "amzn,cannoli",
        fit_configs: CANNOLI_FIT_CONFIGS,
    },
    soc: Soc::Mt8519,
    storage: StorageKind::Ubi,
    // The UBI volumes of a C2N6L4: system_a/b hold 508 LEBs of 258048 bytes.
    // U-Boot reads `bootimage_size` = 0xC1C000 bytes of the 53-LEB boot
    // volume (Amazon's board_r.c), so that is all a FIT may take.
    slots: Slots {
        boot: 0xC1C000,
        root: 131_088_384,
    },
    ring: Ring {
        driver: RingDriver::Is31fl3246Class,
        leds: 12,
        // `ledmod_0` is at the back, over the power socket, and the numbers
        // run clockwise seen from above (TECHO5, one C2N6L4, 2026-11-21), so
        // LED 6 sits at the front. No gap falls at the front centre as on
        // the MT8163 ring: the pair is LED 6 and its clockwise neighbour.
        front_centre: [6, 7],
    },
    keys: ECHO_DOT_5_KEYS,
    privacy: GPIO_PRIVACY,
    audio: Audio {
        card: "mt-snd-card",
        playback: Pcm {
            device: "hw:0,1",
            rate: 96_000,
            channels: 2,
            format: SampleFormat::S16Le,
        },
        capture: Pcm {
            device: "hw:0,9",
            rate: 96_000,
            channels: 6,
            format: SampleFormat::S32Le,
        },
        codec: Codec::Mt8519Max98396,
        speaker: SpeakerTuning::ECHO_DOT_5,
        // One amplifier on eTDM2. Which of DL2's two slots it plays is not
        // measured; pinnad plays a mono mix, so both loopback lanes carry
        // what it plays.
        speaker_channels: SpeakerChannels::Only(Channel::Left),
        // The Echo Dot (2nd gen)'s corner; not measured on this driver.
        speaker_highpass_hz: 150.0,
    },
    mics: ECHO_DOT_5_MICS,
    wifi: Wifi::Mt7663Sdio,
};

/// The Echo Dot (5th gen)'s three microphones: an L with 22.5 mm legs and
/// microphone 1 in the corner. The shape is Amazon's
/// (`codebook_Cancheese.txt`); where it lies was measured by TECHO5 on one
/// C2N6L4 (2026-11-23, white noise from each of the twelve LEDs at 1 m,
/// fitted to 0.7 mm rms), in a frame with x toward LED 0 at the back. Here
/// the same points are turned to Pinna's frame, x toward the front.
const ECHO_DOT_5_MICS: Mics = Mics {
    count: 3,
    // Lane 3 is the converter's unwired fourth input; DL2's left and right
    // channel come back on lanes 4 and 5 (TECHO5, Amazon's 5.4 kernel).
    loopback_at: 4,
    loopback: &[Channel::Left, Channel::Right],
    geometry: MicGeometry::Points(&[(-1.72, -16.68), (10.35, 2.30), (-8.63, 14.38)]),
    speech_mask: 0b111,
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
            .map(|i| self.mics.loopback_at + i)
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
        assert_eq!(Model::EchoDot5.id(), "echo-dot-5");
        assert_eq!(Model::from_id(""), None);
        assert_eq!(Model::from_id("echo-dot-3"), None);
    }

    #[test]
    fn wifi_ids_are_what_the_image_scripts_match() {
        assert_eq!(ECHO_DOT_2.wifi.id(), "mt8163-consys");
        assert_eq!(ECHO_DOT_5.wifi.id(), "mt7663-sdio");
    }

    #[test]
    fn only_the_mac80211_chip_joins_wpa3() {
        assert!(!ECHO_DOT_2.wifi.supports_sae());
        assert!(!ECHO_2.wifi.supports_sae());
        // Unmeasured on the MT7663: not claimed until a WPA3-only join runs.
        assert!(!ECHO_DOT_5.wifi.supports_sae());
    }

    #[test]
    fn names() {
        assert_eq!(Model::EchoDot2.name(), "Echo Dot (2nd gen)");
        assert_eq!(Model::Echo2.name(), "Echo (2nd gen)");
        assert_eq!(Model::EchoDot2.home_assistant_name(), "Echo Dot (2nd Gen)");
        assert_eq!(Model::Echo2.home_assistant_name(), "Echo (2nd Gen)");
        assert_eq!(Model::EchoDot2.short_name(), "Echo Dot");
        assert_eq!(Model::Echo2.short_name(), "Echo");
        assert_eq!(Model::EchoDot5.name(), "Echo Dot (5th gen)");
        assert_eq!(Model::EchoDot5.home_assistant_name(), "Echo Dot (5th Gen)");
        assert_eq!(Model::EchoDot5.short_name(), "Echo Dot");
    }

    #[test]
    fn the_mt8163_echos_boot_from_gpt_and_the_mt8519_from_ubi() {
        for m in Model::ALL {
            let b = m.board();
            let want = match b.soc {
                Soc::Mt8163 => StorageKind::Gpt,
                Soc::Mt8519 => StorageKind::Ubi,
            };
            assert_eq!(b.storage, want, "{m:?}");
        }
        assert_eq!(Soc::Mt8519.compatible(), "mediatek,mt8519");
    }

    #[test]
    fn echo_dot_5_keys() {
        let k = ECHO_DOT_5.keys;
        assert_eq!(button_in(k, KEY_VOLUMEUP), Some(Button::VolumeUp));
        assert_eq!(button_in(k, KEY_VOLUMEDOWN), Some(Button::VolumeDown));
        assert_eq!(button_in(k, KEY_HELP), Some(Button::Action));
        assert_eq!(button_in(k, KEY_MUTE), Some(Button::Mute));
        // No power key: mtk-kpd declares none on this board.
        assert_eq!(button_in(k, KEY_POWER), None);
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
        assert_eq!(SpeakerTuning::named("Flat"), Some(SpeakerTuning::FLAT));
        assert_eq!(SpeakerTuning::named("flat"), Some(SpeakerTuning::FLAT));
        assert_eq!(SpeakerTuning::named("Radar"), Some(SpeakerTuning::RADAR));
        assert_eq!(SpeakerTuning::named("anything"), None);
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
                speaker_highpass_hz: 150.0,
                ..b.audio
            },
            a.audio
        );
    }

    #[test]
    fn mic_lanes_fit_the_capture_pcm() {
        for m in Model::ALL {
            let b = m.board();
            assert_eq!(b.mics.lanes(), b.audio.capture.channels, "{m:?}");
            assert!(b.mics.loopback_at >= b.mics.count, "{m:?}");
            assert!(!b.mics.loopback.is_empty(), "{m:?}");
            assert_eq!(u32::from(b.mics.speech_mask) >> b.mics.count, 0, "{m:?}");
            assert_eq!(b.mics.speech_mask & !b.mics.all(), 0, "{m:?}");
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
    fn the_bootloader_decides_the_boot_format_and_who_counts_tries() {
        assert_eq!(ECHO_DOT_2.bootloader.boot_format(), BootFormat::Android);
        assert_eq!(ECHO_DOT_2.bootloader.fastboot_product(), Some("BISCUIT"));
        assert!(ECHO_2.bootloader.counts_tries());
        assert_eq!(ECHO_DOT_5.bootloader.boot_format(), BootFormat::Fit);
        assert_eq!(ECHO_DOT_5.bootloader.fastboot_product(), None);
        assert!(ECHO_DOT_5.bootloader.counts_tries());
    }

    #[test]
    fn the_echo_dot_5_has_the_smallest_slots() {
        let all = Slots::smallest(&Model::ALL).unwrap();
        assert_eq!(all, ECHO_DOT_5.slots);
        assert_eq!(all.root, 508 * 258_048);
        assert_eq!(
            Slots::smallest(&[Model::EchoDot2, Model::Echo2]),
            Some(Slots::MT8163)
        );
        assert_eq!(Slots::smallest(&[]), None);
    }

    #[test]
    fn echo_dot_5_mics_are_an_l_with_22_5_mm_legs_and_mic_1_in_the_corner() {
        let g = ECHO_DOT_5.mics.geometry;
        let d = |a: usize, b: usize| {
            let ((ax, ay), (bx, by)) = (g.position(a), g.position(b));
            (ax - bx).hypot(ay - by)
        };
        assert!((d(0, 1) - 0.0225).abs() < 0.0001);
        assert!((d(1, 2) - 0.0225).abs() < 0.0001);
        assert!((d(0, 2) - 0.0225 * 2f64.sqrt()).abs() < 0.0002);
        // TECHO5's mic 0 at (1.72, 16.68) mm with x toward the back is here
        // turned half a circle.
        let (x, y) = g.position(0);
        assert!((x + 0.00172).abs() < 1e-9 && (y + 0.01668).abs() < 1e-9);
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

    #[test]
    fn a_fit_board_lists_the_configurations_its_u_boot_boots() {
        let dot5 = Model::EchoDot5.board().bootloader;
        let confs = dot5.fit_configs();
        // The name a restart asks for, and the one a cold start does.
        assert!(confs.contains(&"cannoli-pvt-aluminum-recovery"));
        assert!(confs.contains(&"cannoli-pvt-plastic"));
        let mut unique = confs.to_vec();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), confs.len());
        assert!(Model::EchoDot2.board().bootloader.fit_configs().is_empty());
    }
}
