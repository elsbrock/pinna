//! Cycle LED animations, print button events and mic-mute state.
//! Real hardware is auto-detected; otherwise mocks are used (type `up`,
//! `down`, `action`, `mute` on stdin). `--mock` forces mocks.

use pinna_hal::anim::{Effect, render};
use pinna_hal::buttons::{self, ButtonEvent, MockButtons};
use pinna_hal::ident;
use pinna_hal::led::{LedHandle, LedRing, MockLeds};
use pinna_hal::privacy::{self, MockPrivacy};
use std::path::Path;
use std::time::{Duration, Instant};

const EFFECTS: [(&str, Effect); 11] = [
    ("boot", Effect::Boot),
    ("listening", Effect::Listening),
    ("thinking", Effect::Thinking),
    ("speaking", Effect::Speaking),
    ("error", Effect::Error),
    ("muted", Effect::Muted),
    ("volume 65", Effect::Volume(65)),
    ("timer 0.6", Effect::Timer(0.6)),
    ("timer ringing", Effect::TimerRinging),
    ("idle", Effect::Idle),
    ("volume 100", Effect::Volume(100)),
];
const EFFECT_TIME: Duration = Duration::from_secs(3);

#[tokio::main]
async fn main() {
    let force_mock = std::env::args().any(|a| a == "--mock");
    // Hardware Pinna doesn't know gets the Echo Dot (2nd gen)'s paths.
    let board = pinna_hal::board::detect().unwrap_or(&pinna_hal::board::ECHO_DOT_2);
    println!("board: {:?}", board.model);

    println!("wlan0 mac: {:?}", ident::wlan0_mac());
    println!("miccal: {:?}", ident::read_miccal());

    let real = if force_mock {
        None
    } else {
        pinna_hal::led::open(board, 100).ok()
    };
    let mock = real.is_none();
    let ring: Box<dyn LedRing> = match real {
        Some(r) => r,
        None => {
            println!("no IS31FL3236 found; using mock LEDs/buttons");
            Box::new(MockLeds::new())
        }
    };
    let leds = LedHandle::spawn(ring);

    let mock_privacy = MockPrivacy::new();
    let mut mute_rx;
    let mut btn_rx;
    if mock {
        btn_rx = MockButtons::from_stdin();
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut sub = mock_privacy.subscribe();
        tokio::spawn(async move {
            while sub.changed().await.is_ok() {
                let v = *sub.borrow();
                if tx.send(v).await.is_err() {
                    break;
                }
            }
        });
        mute_rx = rx;
    } else {
        btn_rx = buttons::button_stream(board.keys);
        mute_rx = privacy::privacy_stream_from(privacy::state_paths(&board.privacy));
    }

    let start = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    let mut shown = usize::MAX;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let el = start.elapsed();
                let idx = (el.as_secs() / EFFECT_TIME.as_secs()) as usize % EFFECTS.len();
                let t = el - Duration::from_secs((el.as_secs() / EFFECT_TIME.as_secs()) * EFFECT_TIME.as_secs());
                leds.show(&render(EFFECTS[idx].1, t));
                if idx != shown {
                    shown = idx;
                    eprintln!("\neffect: {}", EFFECTS[idx].0);
                }
            }
            Some(e) = btn_rx.recv() => {
                eprintln!("\nbutton: {e:?}");
                if mock && e == ButtonEvent::Press(buttons::Button::Mute) {
                    mock_privacy.toggle();
                }
            }
            Some(m) = mute_rx.recv() => {
                eprintln!("\nmic privacy: {}", if m { "MUTED" } else { "live" });
                let _ = pinna_hal::led::set_mute_lamp_at(Path::new(board.privacy.mute_lamp), m);
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
}
