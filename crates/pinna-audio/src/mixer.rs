//! ALSA mixer sequences for the Echo Dot, translated from LibreEcho's
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

/// Microphone PGA volume: LibreEcho's default and hard maximum.
pub const MICPGA_DEFAULT: u32 = 40;
pub const MICPGA_MAX: u32 = 80;

const ADC_BANKS: [&str; 4] = ["A", "B", "C", "D"];

/// micd.c `configure_capture_path`, in order, except the DIF1 input gain.
/// `micpga` is clamped to [`MICPGA_MAX`]. The PGA volume writes come last (the
/// C code falls back to [`MICPGA_DEFAULT`] if one is refused; see
/// [`micpga_fallback`]).
///
/// "DIF1 Input Gain" is the low bit of the ADC3101's two-bit routing field for
/// that input: 0 routes it at 0 dB, 1 at -6 dB. micd.c writes 1; TECHO5
/// (`echod/internal/hardware/mic/gain.go`) writes 0. Measured on an Echo Dot
/// (2nd gen), 2026-11-21: writing 0 raised every microphone lane by
/// 5.5-6.2 dB.
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
            &["0"],
        ));
        v.push(MixerCmd::new(
            &format!("ADC_{bank} DIF1_R Input Gain"),
            &["0"],
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

/// The values in `amixer cget` output (`  : values=on,off`), with `on`/`off`
/// as `1`/`0`, the way they are written.
fn parse_values(out: &str) -> Option<Vec<String>> {
    let line = out
        .lines()
        .find_map(|l| l.trim_start().strip_prefix(": values="))?;
    Some(
        line.split(',')
            .map(|v| match v.trim() {
                "on" => "1".to_owned(),
                "off" => "0".to_owned(),
                v => v.to_owned(),
            })
            .collect(),
    )
}

/// Whether a control read as `got` holds what `want` wrote. amixer sets
/// every channel from a single value, so the last wanted value covers the rest.
fn holds(want: &[String], got: &[String]) -> bool {
    !want.is_empty()
        && !got.is_empty()
        && got
            .iter()
            .enumerate()
            .all(|(i, g)| *g == want[i.min(want.len() - 1)])
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
    /// Codec "HP Driver Gain Volume" index (LibreEcho's default, 6).
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

    /// A control's current values, `on`/`off` read as `1`/`0`; `None` if
    /// amixer can't read it.
    async fn read(&self, control: &str) -> Option<Vec<String>> {
        let out = Command::new(&self.bin)
            .args([
                "-c".into(),
                self.card.to_string(),
                "cget".into(),
                format!("name={control}"),
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        parse_values(&String::from_utf8_lossy(&out.stdout))
    }

    /// Read back every command's control and set again the ones that don't
    /// hold the value written. Returns the controls set again.
    ///
    /// The microphone setup needs it: after a boot, the Echo Dot (2nd gen) and
    /// the Echo (2nd gen) both came up with "ADC_A Right Ip Select ADC_A
    /// DIF1_R switch" off although pinnad had written it on, so lane 1 heard
    /// nothing (2026-11-21). Writing it again while capturing brought the lane
    /// back. What switches it off is not known yet.
    pub async fn check(&self, cmds: &[MixerCmd]) -> Vec<String> {
        let mut fixed = Vec::new();
        for cmd in cmds {
            let Some(got) = self.read(&cmd.control).await else {
                continue;
            };
            if holds(&cmd.values, &got)
                || micpga_fallback(cmd).is_some_and(|fb| holds(&fb.values, &got))
            {
                continue;
            }
            tracing::warn!(
                control = %cmd.control,
                got = %got.join(","),
                want = %cmd.values.join(","),
                "mixer control lost its setting; setting it again"
            );
            if !self.run_one(cmd).await {
                tracing::error!(control = %cmd.control, "mixer control failed");
            }
            fixed.push(cmd.control.clone());
        }
        fixed
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
    fn capture_sequence_routes_dif1_at_0_db() {
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
            "ADC_A DIF1_L Input Gain=0",
            "ADC_A DIF1_R Input Gain=0",
            "ADC_B DIF1_L Input Gain=0",
            "ADC_B DIF1_R Input Gain=0",
            "ADC_C DIF1_L Input Gain=0",
            "ADC_C DIF1_R Input Gain=0",
            "ADC_D DIF1_L Input Gain=0",
            "ADC_D DIF1_R Input Gain=0",
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

    #[test]
    fn cget_output_reads_as_written_values() {
        // amixer 1.2 on an Echo (2nd gen), 2026-11-21.
        let out = "numid=60,iface=MIXER,name='ADC_A Right Ip Select ADC_A DIF1_R switch'\n  \
                   ; type=BOOLEAN,access=rw------,values=1\n  : values=off\n";
        assert_eq!(parse_values(out), Some(vec!["0".to_owned()]));
        let out =
            "  ; type=INTEGER,access=rw---R--,values=2,min=0,max=80,step=0\n  : values=40,40\n";
        assert_eq!(parse_values(out), Some(vec!["40".into(), "40".into()]));
        assert_eq!(parse_values("nothing here"), None);
    }

    #[test]
    fn a_single_value_holds_for_every_channel() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(holds(&v(&["1"]), &v(&["1", "1"])));
        assert!(holds(&v(&["40", "40"]), &v(&["40", "40"])));
        assert!(!holds(&v(&["1"]), &v(&["1", "0"])));
        assert!(!holds(&v(&["1"]), &v(&[])));
    }

    #[tokio::test]
    async fn a_control_that_lost_its_setting_is_set_again() {
        use std::os::unix::fs::PermissionsExt;
        // A stand-in amixer keeping each control's values in a file.
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let bin = dir.path().join("amixer");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nop=; name=; val=\n\
                 while [ $# -gt 0 ]; do case \"$1\" in\n\
                 -c) shift;; -q) ;; cset|cget) op=$1;; name=*) name=${{1#name=}};; *) val=$1;;\n\
                 esac; shift; done\n\
                 f='{}'/$(printf %s \"$name\" | tr ' /' '__')\n\
                 if [ \"$op\" = cset ]; then printf %s \"$val\" > \"$f\"; echo \"$name\" >> '{}'/../written;\n\
                 else [ -f \"$f\" ] || exit 1; echo \"  : values=$(cat \"$f\")\"; fi\n",
                state.display(),
                state.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runner = MixerRunner {
            bin: bin.to_str().unwrap().into(),
            card: 0,
        };
        let setup = capture_setup(MICPGA_DEFAULT);
        // Another test's fork may still hold the new script open for a moment.
        for attempt in 0.. {
            match runner.apply(&setup).await {
                Ok(()) => break,
                Err(_) if attempt < 20 => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await
                }
                Err(e) => panic!("{e}"),
            }
        }
        // After boot, lane 1's input reads off although it was written on.
        let lost = "ADC_A Right Ip Select ADC_A DIF1_R switch";
        std::fs::write(state.join(lost.replace(' ', "_")), "off").unwrap();
        std::fs::remove_file(dir.path().join("written")).unwrap();

        assert_eq!(runner.check(&setup).await, vec![lost.to_owned()]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("written")).unwrap(),
            format!("{lost}\n"),
            "only the lost control is written again"
        );
        assert!(runner.check(&setup).await.is_empty());
    }
}
