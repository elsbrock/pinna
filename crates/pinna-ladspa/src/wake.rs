//! LADSPA plugin `pinna_wake`: the wake word's microphone stream (ADR 0033).
//!
//! Inputs are microphone lanes 0-6 and loopback lane 7, which carries what the
//! speaker plays in the same frame as the microphones. The output is the mean
//! of the calibrated microphones in the mask, minus the linear echo
//! canceller's estimate from lane 7 (`pinna_audio::echocancel`), then the
//! beam's 80 Hz high-pass and digital gain. There is no suppressor, so a
//! talker over playback reaches the wake word detector.

use crate::ladspa::*;
use crate::plugin::{
    AUDIO_IN, BOUNDED, CONTROL_IN, DEFAULT_GAIN_DB, SAMPLE_RATE, SyncPtrs, control_int,
    gain_hundredths, hint, to_i16,
};
use pinna_audio::dsp::{
    Calibration, HighPass, LOGICAL_CHANNELS, MIC_MASK_ALL, MIC_MASK_BEAM, apply_digital_gain,
    saturate_s16,
};
use pinna_audio::echocancel::LinearEchoCanceller;
use std::os::raw::{c_char, c_int, c_ulong};
use std::panic::catch_unwind;
use std::ptr;

/// The beam plugin's ID plus one.
pub const UNIQUE_ID: u32 = 0x0050_494F;
pub const LABEL: &str = "pinna_wake";
/// The microphones averaged: the beam's four.
pub const DEFAULT_MIC_MASK: u8 = MIC_MASK_BEAM;

/// Microphone lanes 0-6, then the loopback lane.
const INPUTS: usize = LOGICAL_CHANNELS + 1;
const PORT_REF: usize = LOGICAL_CHANNELS;
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
    c"Ref".as_ptr(),
    c"Out".as_ptr(),
    c"Gain dB".as_ptr(),
    c"Mic mask".as_ptr(),
    c"Calibrate".as_ptr(),
]);

// As for the beam, the graph sets Gain dB and Mic mask exactly via `control`.
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
    hint(BOUNDED | HINT_DEFAULT_MIDDLE, 0.0, 30.0),
    hint(
        BOUNDED | HINT_INTEGER | HINT_DEFAULT_LOW,
        1.0,
        MIC_MASK_ALL as f32,
    ),
    hint(HINT_TOGGLED | HINT_DEFAULT_1, 0.0, 1.0),
];

pub(crate) static DESCRIPTOR: SyncPtrs<Descriptor> = SyncPtrs(Descriptor {
    unique_id: UNIQUE_ID as c_ulong,
    label: c"pinna_wake".as_ptr(),
    properties: PROPERTY_HARD_RT_CAPABLE,
    name: c"Pinna Echo Dot wake word stream".as_ptr(),
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

struct Wake {
    ports: [*mut Data; PORT_COUNT],
    calibration: Calibration,
    canceller: Box<LinearEchoCanceller>,
    highpass: HighPass,
    lanes: [usize; LOGICAL_CHANNELS],
    lane_count: usize,
    gain_db: f32,
    gain_hundredths: u32,
}

impl Wake {
    fn set_mask(&mut self, mask: u8) {
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
        let mut wake = Box::new(Wake {
            ports: [ptr::null_mut(); PORT_COUNT],
            calibration: Calibration::new(pinna_hal::ident::read_miccal()),
            canceller: Box::new(LinearEchoCanceller::new()),
            highpass: HighPass::new(),
            lanes: [0; LOGICAL_CHANNELS],
            lane_count: 0,
            gain_db: DEFAULT_GAIN_DB,
            gain_hundredths: gain_hundredths(DEFAULT_GAIN_DB),
        });
        wake.set_mask(DEFAULT_MIC_MASK);
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
            .map_or(DEFAULT_MIC_MASK, |v| {
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
    for i in 0..n as usize {
        let mut sum = 0f32;
        for &lane in &p.lanes[..p.lane_count] {
            // SAFETY: the host guarantees `n` valid samples on connected audio
            // ports; raw reads keep an in-place host (out aliasing an input) sound.
            let s = to_i16(unsafe { *p.ports[lane].add(i) });
            let s = if calibrate {
                p.calibration.apply(lane, s)
            } else {
                s
            };
            sum += f32::from(s);
        }
        let mic = sum / p.lane_count as f32;
        // SAFETY: as above.
        let reference = f32::from(to_i16(unsafe { *p.ports[PORT_REF].add(i) }));
        let e = p.canceller.process(mic, reference);
        let mut y = [p.highpass.sample(saturate_s16(e.round() as i64))];
        apply_digital_gain(&mut y, p.gain_hundredths);
        // SAFETY: as above.
        unsafe { *out.add(i) = f32::from(y[0]) / 32768.0 };
    }
}

unsafe extern "C" fn cleanup(h: Handle) {
    if !h.is_null() {
        // SAFETY: `h` came from `instantiate` and is not used afterwards.
        let _ = catch_unwind(|| drop(unsafe { Box::from_raw(h.cast::<Wake>()) }));
    }
}
