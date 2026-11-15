//! Print the Echo Dot mixer sequences as `amixer` commands, for running them by
//! hand on a device (e.g. while experimenting with the audio graph).
//!
//!   cargo run -p pinna-audio --example mixer_script -- capture|playback-on|playback-off

use pinna_audio::mixer::{
    MICPGA_DEFAULT, SpeakerProfile, capture_setup, playback_arm, playback_disable, playback_power,
    playback_unmute,
};

fn main() {
    let which = std::env::args().nth(1).unwrap_or_default();
    let profile = SpeakerProfile {
        codec_flat: true,
        ..Default::default()
    };
    let cmds = match which.as_str() {
        "capture" => capture_setup(MICPGA_DEFAULT),
        "playback-on" => [playback_arm(&profile), playback_power(), playback_unmute()].concat(),
        "playback-off" => playback_disable(),
        _ => {
            eprintln!("usage: mixer_script capture|playback-on|playback-off");
            std::process::exit(2);
        }
    };
    for c in cmds {
        let args: Vec<String> = c
            .amixer_args(0)
            .into_iter()
            .map(|a| format!("'{}'", a.replace('\'', r"'\''")))
            .collect();
        let tail = if c.optional { " || true" } else { "" };
        println!("amixer {}{tail}", args.join(" "));
    }
}
