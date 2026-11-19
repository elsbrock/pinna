//! ALSA mixer sequences for the Echo Dot, translated from the previous firmware's
//! tinymix / tinyalsa calls (micd.c `configure_capture_path`, audio_engine.c
//! `arm_output_controls` and friends) into `amixer cset` invocations.
//!
//! Value syntax: tinymix takes one argument per channel (`set "ctl" 1 1`);
//! amixer takes one comma-separated list (`cset name='ctl' 1,1`). Enumerated
//! controls take their item name in both tools (`On`, `Stereo`, `Flat`...).

use crate::{Error, Result};
use std::process::Stdio;
use tokio::process::Command;

/// One control write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MixerCmd {
    pub control: String,
    /// One entry per channel (all identical for tinymix's "same value" case).
    pub values: Vec<String>,
    /// Failure is logged but does not fail the sequence.
    pub optional: bool,
}

impl MixerCmd {
    pub fn new(control: &str, values: &[&str]) -> Self {
        MixerCmd {
            control: control.to_string(),
            values: values.iter().map(|v| v.to_string()).collect(),
            optional: false,
        }
    }

    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Arguments for `amixer`: `-c <card> cset name=<ctl> <v1,v2>`.
    pub fn amixer_args(&self, card: u32) -> Vec<String> {
        vec![
            "-q".into(),
            "-c".into(),
            card.to_string(),
            "cset".into(),
            format!("name={}", self.control),
            self.values.join(","),
        ]
    }
}

/// Microphone PGA volume: the previous firmware's default and hard maximum.
pub const MICPGA_DEFAULT: u32 = 40;
pub const MICPGA_MAX: u32 = 80;

const ADC_BANKS: [&str; 4] = ["A", "B", "C", "D"];

/// micd.c `configure_capture_path`, in order. `micpga` is clamped to
/// [`MICPGA_MAX`]. The PGA volume writes come last (the C code falls back to
/// [`MICPGA_DEFAULT`] if one is refused; see [`micpga_fallback`]).
pub fn capture_setup(micpga: u32) -> Vec<MixerCmd> {
    let mut v = vec![
        MixerCmd::new("Mic PGA Switch", &["1", "1"]),
        MixerCmd::new("ADCFGA Left Mute Switch", &["0"]),
        MixerCmd::new("ADCFGA Right Mute Switch", &["0"]),
    ];
    for bank in ADC_BANKS {
        v.push(MixerCmd::new(
            &format!("ADC_{bank} Left Ip Select ADC_{bank} DIF1_L switch"),
            &["1"],
        ));
        v.push(MixerCmd::new(
            &format!("ADC_{bank} Right Ip Select ADC_{bank} DIF1_R switch"),
            &["1"],
        ));
    }
    for bank in ADC_BANKS {
        v.push(MixerCmd::new(
            &format!("ADC_{bank} DIF1_L Input Gain"),
            &["1"],
        ));
        v.push(MixerCmd::new(
            &format!("ADC_{bank} DIF1_R Input Gain"),
            &["1"],
        ));
    }
    v.push(MixerCmd::new("SpiTimeStamps", &["0"]));
    let pga = micpga.min(MICPGA_MAX).to_string();
    for bank in ADC_BANKS {
        v.push(MixerCmd::new(
            &format!("ADC_{bank} MICPGA Volume Ctrl"),
            &[&pga, &pga],
        ));
    }
    v
}

/// The default-level replacement for a refused PGA write.
pub fn micpga_fallback(cmd: &MixerCmd) -> Option<MixerCmd> {
    if !cmd.control.ends_with("MICPGA Volume Ctrl") {
        return None;
    }
    let d = MICPGA_DEFAULT.to_string();
    Some(MixerCmd::new(&cmd.control, &[&d, &d]))
}

/// Per-target speaker settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeakerProfile {
    /// Kernel "Speaker Codec Profile": Flat for biscuit (Echo Dot), Radar for
    /// the Echo 2nd gen.
    pub codec_flat: bool,
    /// Codec "HP Driver Gain Volume" index (the previous firmware's default, 6).
    pub hp_driver_gain: u32,
    /// How long a frame takes from the playback PCM's hardware pointer to
    /// the air: the AFE's FIFO, I2S, the codec and the amplifier. A timed
    /// source adds it to the time a frame is heard (ADR 0027 §10). It is to
    /// be measured on each model, against a reference player, and recorded
    /// with that evidence; until then it is 0.
    pub output_delay: std::time::Duration,
}

impl Default for SpeakerProfile {
    fn default() -> Self {
        SpeakerProfile {
            codec_flat: true,
            hp_driver_gain: 6,
            output_delay: std::time::Duration::ZERO,
        }
    }
}

/// audio_engine.c `arm_output_controls`: everything set up with the amp off and
/// the codec muted, before a stream exists. `PCM Playback Volume` is pinned at
/// raw index 127 (0 dB); software owns gain.
pub fn playback_arm(profile: &SpeakerProfile) -> Vec<MixerCmd> {
    let gain = profile.hp_driver_gain.to_string();
    vec![
        MixerCmd::new("MFP Gpio Mute", &["On"]),
        MixerCmd::new("Ext_Speaker_Amp_Switch", &["Off"]),
        MixerCmd::new("Audio_DacMux_Setting", &["Off"]),
        MixerCmd::new("Board Channel Config", &["Stereo"]),
        MixerCmd::new("Right Channel Only", &["Off"]),
        MixerCmd::new("HP DAC Playback Switch", &["1", "1"]),
        MixerCmd::new("HPL Output Mixer L_DAC Switch", &["1"]),
        MixerCmd::new("HPR Output Mixer R_DAC Switch", &["1"]),
        MixerCmd::new("HPR Output Mixer IN1_R Switch", &["0"]),
        MixerCmd::new("HP Driver Gain Volume", &[&gain, &gain]),
        // Older kernels lack the control; Radar is their implicit default.
        MixerCmd::new(
            "Speaker Codec Profile",
            &[if profile.codec_flat { "Flat" } else { "Radar" }],
        )
        .optional(),
        MixerCmd::new("PCM Playback Volume", &["127", "127"]),
    ]
}

/// `power_output_controls`: amp on while the codec stays muted. The caller
/// waits [`AMP_SETTLE`] afterwards.
pub fn playback_power() -> Vec<MixerCmd> {
    vec![
        MixerCmd::new("MFP Gpio Mute", &["On"]),
        MixerCmd::new("Ext_Speaker_Amp_Switch", &["On"]),
    ]
}

pub const AMP_SETTLE: std::time::Duration = std::time::Duration::from_millis(30);

/// `unmute_output_controls`.
pub fn playback_unmute() -> Vec<MixerCmd> {
    vec![MixerCmd::new("MFP Gpio Mute", &["Off"])]
}

/// `disable_output_controls`: mute before removing amp power (no pop).
pub fn playback_disable() -> Vec<MixerCmd> {
    vec![
        MixerCmd::new("MFP Gpio Mute", &["On"]),
        MixerCmd::new("Ext_Speaker_Amp_Switch", &["Off"]),
    ]
}

/// Runs mixer command lists through `amixer`.
#[derive(Clone, Debug)]
pub struct MixerRunner {
    pub bin: String,
    pub card: u32,
}

impl Default for MixerRunner {
    fn default() -> Self {
        MixerRunner {
            bin: "amixer".into(),
            card: 0,
        }
    }
}

impl MixerRunner {
    async fn run_one(&self, cmd: &MixerCmd) -> bool {
        let status = Command::new(&self.bin)
            .args(cmd.amixer_args(self.card))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        matches!(status, Ok(s) if s.success())
    }

    /// Apply every command in order. All are attempted; a failed non-optional
    /// one makes the result an error listing the controls.
    pub async fn apply(&self, cmds: &[MixerCmd]) -> Result<()> {
        let mut failed = Vec::new();
        for cmd in cmds {
            if self.run_one(cmd).await {
                continue;
            }
            if let Some(fb) = micpga_fallback(cmd) {
                tracing::warn!(control = %cmd.control, "PGA value refused; using default");
                if self.run_one(&fb).await {
                    continue;
                }
            }
            if cmd.optional {
                tracing::warn!(control = %cmd.control, "optional mixer control unavailable");
            } else {
                tracing::error!(control = %cmd.control, "mixer control failed");
                failed.push(cmd.control.clone());
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(Error::Mixer(failed.join(", ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(cmds: &[MixerCmd]) -> Vec<String> {
        cmds.iter()
            .map(|c| format!("{}={}", c.control, c.values.join(" ")))
            .collect()
    }

    #[test]
    fn capture_sequence_matches_micd_c() {
        let got = flat(&capture_setup(40));
        let want = [
            "Mic PGA Switch=1 1",
            "ADCFGA Left Mute Switch=0",
            "ADCFGA Right Mute Switch=0",
            "ADC_A Left Ip Select ADC_A DIF1_L switch=1",
            "ADC_A Right Ip Select ADC_A DIF1_R switch=1",
            "ADC_B Left Ip Select ADC_B DIF1_L switch=1",
            "ADC_B Right Ip Select ADC_B DIF1_R switch=1",
            "ADC_C Left Ip Select ADC_C DIF1_L switch=1",
            "ADC_C Right Ip Select ADC_C DIF1_R switch=1",
            "ADC_D Left Ip Select ADC_D DIF1_L switch=1",
            "ADC_D Right Ip Select ADC_D DIF1_R switch=1",
            "ADC_A DIF1_L Input Gain=1",
            "ADC_A DIF1_R Input Gain=1",
            "ADC_B DIF1_L Input Gain=1",
            "ADC_B DIF1_R Input Gain=1",
            "ADC_C DIF1_L Input Gain=1",
            "ADC_C DIF1_R Input Gain=1",
            "ADC_D DIF1_L Input Gain=1",
            "ADC_D DIF1_R Input Gain=1",
            "SpiTimeStamps=0",
            "ADC_A MICPGA Volume Ctrl=40 40",
            "ADC_B MICPGA Volume Ctrl=40 40",
            "ADC_C MICPGA Volume Ctrl=40 40",
            "ADC_D MICPGA Volume Ctrl=40 40",
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn micpga_never_exceeds_80() {
        let c = capture_setup(500);
        assert_eq!(c.last().unwrap().values, vec!["80", "80"]);
    }

    #[test]
    fn micpga_fallback_is_default_only_for_pga() {
        let c = capture_setup(60);
        let pga = c.last().unwrap();
        assert_eq!(micpga_fallback(pga).unwrap().values, vec!["40", "40"]);
        assert!(micpga_fallback(&c[0]).is_none());
    }

    #[test]
    fn amixer_args_translation() {
        let c = MixerCmd::new("Mic PGA Switch", &["1", "1"]);
        assert_eq!(
            c.amixer_args(0),
            ["-q", "-c", "0", "cset", "name=Mic PGA Switch", "1,1"]
        );
    }

    #[test]
    fn arm_sequence_matches_audio_engine_c() {
        let got = flat(&playback_arm(&SpeakerProfile::default()));
        let want = [
            "MFP Gpio Mute=On",
            "Ext_Speaker_Amp_Switch=Off",
            "Audio_DacMux_Setting=Off",
            "Board Channel Config=Stereo",
            "Right Channel Only=Off",
            "HP DAC Playback Switch=1 1",
            "HPL Output Mixer L_DAC Switch=1",
            "HPR Output Mixer R_DAC Switch=1",
            "HPR Output Mixer IN1_R Switch=0",
            "HP Driver Gain Volume=6 6",
            "Speaker Codec Profile=Flat",
            "PCM Playback Volume=127 127",
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn radar_profile_and_optional_flag() {
        let cmds = playback_arm(&SpeakerProfile {
            codec_flat: false,
            hp_driver_gain: 9,
            ..Default::default()
        });
        let prof = cmds
            .iter()
            .find(|c| c.control == "Speaker Codec Profile")
            .unwrap();
        assert_eq!(prof.values, vec!["Radar"]);
        assert!(prof.optional);
        assert_eq!(cmds[9].values, vec!["9", "9"]);
        assert!(cmds.iter().filter(|c| c.optional).count() == 1);
    }

    #[test]
    fn power_unmute_disable_sequences() {
        assert_eq!(
            flat(&playback_power()),
            ["MFP Gpio Mute=On", "Ext_Speaker_Amp_Switch=On"]
        );
        assert_eq!(flat(&playback_unmute()), ["MFP Gpio Mute=Off"]);
        assert_eq!(
            flat(&playback_disable()),
            ["MFP Gpio Mute=On", "Ext_Speaker_Amp_Switch=Off"]
        );
        assert_eq!(AMP_SETTLE.as_millis(), 30);
    }
}
