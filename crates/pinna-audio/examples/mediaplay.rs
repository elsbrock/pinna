//! Media lane smoke test on the PipeWire backend.
//!
//!   mediaplay <url-or-path>
//!
//! Streams the URL; type `p` (pause), `r` (resume), `m` (mute toggle),
//! `d` (hold duck toggle), `t` (play a beep to hear ducking), `s` (stop),
//! `q` (quit) and Enter. `PINNA_PW_TARGET` selects a PipeWire node.

use pinna_audio::speaker::SpeakerConfig;
use pinna_audio::{Backend, Speaker, tone};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let Some(url) = std::env::args().nth(1) else {
        eprintln!("usage: mediaplay <url-or-path>");
        std::process::exit(2);
    };
    let mut cfg = SpeakerConfig::new(Backend::PipeWire);
    cfg.pw_target = std::env::var("PINNA_PW_TARGET").ok();
    cfg.initial_volume = 80;
    let speaker = Speaker::new(cfg);
    let media = speaker.media();

    let mut state = media.state();
    tokio::spawn(async move {
        while state.changed().await.is_ok() {
            eprintln!("media: {:?}", *state.borrow());
        }
    });

    if let Err(e) = media.play_url(&url).await {
        eprintln!("play failed: {e}");
        std::process::exit(1);
    }
    let (mut muted, mut ducked) = (false, false);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        match line.trim() {
            "p" => media.pause(),
            "r" => media.resume(),
            "m" => {
                muted = !muted;
                media.set_muted(muted);
            }
            "d" => {
                ducked = !ducked;
                speaker.duck(ducked);
            }
            "t" => {
                let sp = speaker.clone();
                tokio::spawn(async move { sp.play_pcm(tone::beep(660.0, 1500, 0.5)).await });
            }
            "s" => media.stop(),
            "q" => break,
            _ => eprintln!("p r m d t s q"),
        }
    }
}
