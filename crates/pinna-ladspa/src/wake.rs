//! LADSPA plugin `pinna_wake`: the satellite's microphone stream (ADR 0004,
//! ADR 0033).
//!
//! Its inputs are all the capture lanes, as `dot_mics` delivers them. The
//! board description (ADR 0032), read when PipeWire loads the plugin, says
//! which lanes are microphones and which loopback lane carries what the
//! speaker plays; hardware Pinna doesn't know gets the Echo Dot (2nd gen)'s.
//! The output is the mean of the calibrated microphones in the mask, minus the
//! linear echo canceller's estimate from that loopback lane
//! (`pinna_audio::echocancel`), then the 80 Hz high-pass and digital gain.
//! There is no suppressor, so a talker over playback reaches the wake word
//! detector and Home Assistant.
//!
//! The plugin also runs the beamformer on the board's speech microphones, for
//! the direction only: after every cycle it publishes the beams' state to
//! `/run/pipewire/pinna-beam` (`pinna_audio::beamview`), which pinnad reads to
//! point the listening ring at the talker. Its audio goes nowhere.

use crate::ladspa::*;
use crate::plugin::{
    AUDIO_IN, BOUNDED, CONTROL_IN, DEFAULT_GAIN_DB, SAMPLE_RATE, SyncPtrs, control_int,
    gain_hundredths, hint, to_i16,
};
use pinna_audio::beamformer::{Beamformer, DEFAULT_BEAMS};
use pinna_audio::beamview::{self, BeamViewWriter};
use pinna_audio::dsp::{
    Calibration, HighPass, LOGICAL_CHANNELS, MIC_MASK_ALL, apply_digital_gain, saturate_s16,
};
use pinna_audio::echocancel::LinearEchoCanceller;
use pinna_hal::board::{Board, ECHO_DOT_2};
use std::os::raw::{c_char, c_int, c_ulong};
use std::panic::catch_unwind;
use std::path::Path;
use std::ptr;

/// The beam plugin's ID plus one.
pub const UNIQUE_ID: u32 = 0x0050_494F;
pub const LABEL: &str = "pinna_wake";

/// Capture lanes the plugin takes: the MT8163 Echos' nine. A board with more
/// fails `every_board_fits_the_plugin` until the plugin takes them.
pub const INPUTS: usize = 9;
const PORT_OUT: usize = INPUTS;
const PORT_GAIN: usize = PORT_OUT + 1;
const PORT_MASK: usize = PORT_GAIN + 1;
const PORT_CALIBRATE: usize = PORT_MASK + 1;
const PORT_COUNT: usize = PORT_CALIBRATE + 1;

static PORT_DESCRIPTORS: [c_int; PORT_COUNT] = [
    AUDIO_IN,
    AUDIO_IN,
    AUDIO_IN,
    AUDIO_IN,
    AUDIO_IN,
    AUDIO_IN,
    AUDIO_IN,
    AUDIO_IN,
    AUDIO_IN,
    PORT_OUTPUT | PORT_AUDIO,
    CONTROL_IN,
    CONTROL_IN,
    CONTROL_IN,
];

static PORT_NAMES: SyncPtrs<[*const c_char; PORT_COUNT]> = SyncPtrs([
    c"In 0".as_ptr(),
    c"In 1".as_ptr(),
    c"In 2".as_ptr(),
    c"In 3".as_ptr(),
    c"In 4".as_ptr(),
    c"In 5".as_ptr(),
    c"In 6".as_ptr(),
    c"In 7".as_ptr(),
    c"In 8".as_ptr(),
    c"Out".as_ptr(),
    c"Gain dB".as_ptr(),
    c"Mic mask".as_ptr(),
    c"Calibrate".as_ptr(),
]);

// The graph sets Gain dB exactly via `control`. Mic mask defaults to every
// lane; the board's microphones bound it.
static PORT_HINTS: [PortRangeHint; PORT_COUNT] = [
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(0, 0.0, 0.0),
    hint(BOUNDED | HINT_DEFAULT_MIDDLE, 0.0, 30.0),
    hint(
        BOUNDED | HINT_INTEGER | HINT_DEFAULT_MAXIMUM,
        1.0,
        MIC_MASK_ALL as f32,
    ),
    hint(HINT_TOGGLED | HINT_DEFAULT_1, 0.0, 1.0),
];

pub(crate) static DESCRIPTOR: SyncPtrs<Descriptor> = SyncPtrs(Descriptor {
    unique_id: UNIQUE_ID as c_ulong,
    label: c"pinna_wake".as_ptr(),
    properties: PROPERTY_HARD_RT_CAPABLE,
    name: c"Pinna microphone stream".as_ptr(),
    maker: c"Pinna".as_ptr(),
    copyright: concat!(env!("CARGO_PKG_LICENSE"), "\0").as_ptr().cast(),
    port_count: PORT_COUNT as c_ulong,
    port_descriptors: PORT_DESCRIPTORS.as_ptr(),
    port_names: PORT_NAMES.0.as_ptr(),
    port_range_hints: PORT_HINTS.as_ptr(),
    implementation_data: ptr::null_mut(),
    instantiate: Some(instantiate),
    connect_port: Some(connect_port),
    activate: Some(activate),
    run: Some(run),
    run_adding: None,
    set_run_adding_gain: None,
    deactivate: None,
    cleanup: Some(cleanup),
});

/// What the plugin takes from the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lanes {
    /// Every microphone, as a lane mask.
    pub mics: u8,
    /// The microphones the beamformer listens to for the direction.
    pub speech: u8,
    /// The loopback lanes the reference averages: the one the speaker plays,
    /// or all of them when it plays both channels.
    pub reference: [usize; 2],
    pub reference_count: usize,
}

impl Lanes {
    pub fn of(board: &Board) -> Lanes {
        let mics = &board.mics;
        let (reference, reference_count) = match board.speaker_lane() {
            Some(lane) => ([lane, lane], 1),
            None => (
                [mics.count, mics.lanes() - 1],
                mics.loopback.len().clamp(1, 2),
            ),
        };
        Lanes {
            mics: mics.all(),
            speech: mics.speech_mask,
            reference,
            reference_count,
        }
    }
}

struct Wake {
    ports: [*mut Data; PORT_COUNT],
    board: Lanes,
    calibration: Calibration,
    canceller: Box<LinearEchoCanceller>,
    highpass: HighPass,
    /// For the ring's direction only.
    beamformer: Box<Beamformer>,
    /// `None` where the snapshot can't be created (PipeWire off the device).
    view: Option<BeamViewWriter>,
    lanes: [usize; LOGICAL_CHANNELS],
    lane_count: usize,
    gain_db: f32,
    gain_hundredths: u32,
}

impl Wake {
    fn set_mask(&mut self, mask: u8) {
        let mask = mask & self.board.mics;
        self.lane_count = 0;
        for lane in 0..LOGICAL_CHANNELS {
            if mask & (1 << lane) != 0 {
                self.lanes[self.lane_count] = lane;
                self.lane_count += 1;
            }
        }
    }
}

unsafe extern "C" fn instantiate(_d: *const Descriptor, rate: c_ulong) -> Handle {
    if rate != c_ulong::from(SAMPLE_RATE) {
        return ptr::null_mut();
    }
    catch_unwind(|| {
        let board = Lanes::of(pinna_hal::board::detect().unwrap_or(&ECHO_DOT_2));
        let beamformer = Beamformer::new(board.speech, DEFAULT_BEAMS)
            .unwrap_or_else(|_| Beamformer::default_echo_dot());
        let mut wake = Box::new(Wake {
            ports: [ptr::null_mut(); PORT_COUNT],
            board,
            calibration: Calibration::new(pinna_hal::ident::read_miccal()),
            canceller: Box::new(LinearEchoCanceller::new()),
            highpass: HighPass::new(),
            beamformer: Box::new(beamformer),
            view: BeamViewWriter::create(Path::new(beamview::PATH)).ok(),
            lanes: [0; LOGICAL_CHANNELS],
            lane_count: 0,
            gain_db: DEFAULT_GAIN_DB,
            gain_hundredths: gain_hundredths(DEFAULT_GAIN_DB),
        });
        wake.set_mask(board.mics);
        Box::into_raw(wake).cast()
    })
    .unwrap_or(ptr::null_mut())
}

unsafe extern "C" fn connect_port(h: Handle, port: c_ulong, data: *mut Data) {
    let p = h.cast::<Wake>();
    if p.is_null() {
        return;
    }
    // SAFETY: `h` came from `instantiate`; the host serialises calls on it.
    if let Some(slot) = unsafe { (*p).ports.get_mut(port as usize) } {
        *slot = data;
    }
}

unsafe extern "C" fn activate(h: Handle) {
    let p = h.cast::<Wake>();
    if p.is_null() {
        return;
    }
    let _ = catch_unwind(|| {
        // SAFETY: `h` came from `instantiate`; the host serialises calls on it.
        let p = unsafe { &mut *p };
        let ptr = p.ports[PORT_MASK];
        // SAFETY: control ports are connected by the host before activate.
        let mask = (!ptr.is_null())
            .then(|| unsafe { *ptr })
            .map_or(MIC_MASK_ALL, |v| {
                control_int(v, 1, usize::from(MIC_MASK_ALL)) as u8
            });
        p.set_mask(mask);
        *p.canceller = LinearEchoCanceller::new();
        p.highpass = HighPass::new();
    });
}

unsafe extern "C" fn run(h: Handle, n: c_ulong) {
    let p = h.cast::<Wake>();
    if p.is_null() {
        return;
    }
    // SAFETY: `h` came from `instantiate`; the host serialises calls on it.
    let p = unsafe { &mut *p };
    if p.ports.iter().any(|q| q.is_null()) || p.lane_count == 0 {
        return;
    }
    // SAFETY: all control ports are connected (checked above).
    let (gain_db, calibrate) = unsafe { (*p.ports[PORT_GAIN], *p.ports[PORT_CALIBRATE] > 0.5) };
    if gain_db != p.gain_db {
        p.gain_db = gain_db;
        p.gain_hundredths = gain_hundredths(gain_db);
    }
    let out = p.ports[PORT_OUT];
    let (reference, reference_count) = (p.board.reference, p.board.reference_count);
    for i in 0..n as usize {
        let mut logical = [0i16; LOGICAL_CHANNELS];
        for (lane, l) in logical.iter_mut().enumerate() {
            // SAFETY: the host guarantees `n` valid samples on connected audio
            // ports; raw reads keep an in-place host (out aliasing an input) sound.
            let s = to_i16(unsafe { *p.ports[lane].add(i) });
            *l = if calibrate {
                p.calibration.apply(lane, s)
            } else {
                s
            };
        }
        let sum: f32 = p.lanes[..p.lane_count]
            .iter()
            .map(|&lane| f32::from(logical[lane]))
            .sum();
        let mic = sum / p.lane_count as f32;
        let r: f32 = reference[..reference_count]
            .iter()
            // SAFETY: as above.
            .map(|&lane| f32::from(to_i16(unsafe { *p.ports[lane].add(i) })))
            .sum();
        let e = p.canceller.process(mic, r / reference_count as f32);
        let mut y = [p.highpass.sample(saturate_s16(e.round() as i64))];
        apply_digital_gain(&mut y, p.gain_hundredths);
        p.beamformer.sample(&logical);
        // SAFETY: as above.
        unsafe { *out.add(i) = f32::from(y[0]) / 32768.0 };
    }
    if let Some(view) = &p.view {
        view.publish(&p.beamformer);
    }
}

unsafe extern "C" fn cleanup(h: Handle) {
    if !h.is_null() {
        // SAFETY: `h` came from `instantiate` and is not used afterwards.
        let _ = catch_unwind(|| drop(unsafe { Box::from_raw(h.cast::<Wake>()) }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pinna_hal::board::Model;

    /// The plugin's ports and the beamformer's array hold every board's lanes.
    #[test]
    fn every_board_fits_the_plugin() {
        for m in Model::ALL {
            let b = m.board();
            assert!(b.mics.lanes() <= INPUTS, "{m:?}");
            assert!(b.mics.count <= LOGICAL_CHANNELS, "{m:?}");
            let lanes = Lanes::of(b);
            assert!(
                lanes
                    .reference
                    .iter()
                    .all(|&l| l >= b.mics.count && l < INPUTS)
            );
            assert!(
                Beamformer::new(lanes.speech, DEFAULT_BEAMS).is_ok(),
                "{m:?}"
            );
        }
    }

    #[test]
    fn the_mt8163_echos_average_all_seven_and_cancel_from_lane_8() {
        for m in Model::ALL {
            let lanes = Lanes::of(m.board());
            assert_eq!(lanes.mics, 0x7f);
            assert_eq!((lanes.reference[0], lanes.reference_count), (8, 1));
        }
    }
}
