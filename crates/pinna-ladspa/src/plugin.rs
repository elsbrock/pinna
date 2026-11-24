use crate::ladspa::*;
use pinna_audio::beamformer::{Beamformer, DEFAULT_BEAMS as BF_DEFAULT_BEAMS, MAX_BEAMS};
use pinna_audio::beamview::{self, BeamViewWriter};
use pinna_audio::dsp::{
    Calibration, DIGITAL_GAIN_MAX, DIGITAL_GAIN_UNITY, HighPass, LOGICAL_CHANNELS, MIC_MASK_ALL,
    MIC_MASK_BEAM, apply_digital_gain, saturate_s16,
};
use std::os::raw::{c_char, c_int, c_ulong};
use std::panic::catch_unwind;
use std::path::Path;
use std::ptr;

/// The only supported sample rate.
pub const SAMPLE_RATE: u32 = 16_000;
/// LADSPA unique ID. 0x50494E ("PIN") sits in the unofficial range: IDs
/// 1-1000 are reserved for private/testing use and nothing registered with
/// ladspa.org uses this one, so it is fixed here and never changes.
pub const UNIQUE_ID: u32 = 0x0050_494E;
pub const LABEL: &str = "pinna_beam";
/// The microphone array `Array` selects: the MT8163 Echos' seven-microphone
/// ring, or the Echo Dot (5th gen)'s three-microphone L.
pub const ARRAY_RING: usize = 0;
pub const ARRAY_ECHO_DOT_5: usize = 1;
/// 20 * log10(4): the x4 (400 hundredths) digital gain pinnad uses.
pub const DEFAULT_GAIN_DB: f32 = 12.041_2;
pub const DEFAULT_MIC_MASK: u8 = MIC_MASK_BEAM;
pub const DEFAULT_BEAMS: usize = BF_DEFAULT_BEAMS;

const PORT_GAIN: usize = LOGICAL_CHANNELS + 1;
const PORT_MASK: usize = PORT_GAIN + 1;
const PORT_BEAMS: usize = PORT_MASK + 1;
const PORT_CALIBRATE: usize = PORT_BEAMS + 1;
const PORT_ARRAY: usize = PORT_CALIBRATE + 1;
const PORT_COUNT: usize = PORT_ARRAY + 1;
const PORT_OUT: usize = LOGICAL_CHANNELS;

pub(crate) const AUDIO_IN: c_int = PORT_INPUT | PORT_AUDIO;
pub(crate) const CONTROL_IN: c_int = PORT_INPUT | PORT_CONTROL;
pub(crate) const BOUNDED: c_int = HINT_BOUNDED_BELOW | HINT_BOUNDED_ABOVE;

// LADSPA default hints cannot express every default exactly (only
// min/low/middle/high/max/0/1/100/440), so Gain dB and Mic mask carry the
// nearest hint; the filter-chain config sets the real values via `control`.
static PORT_DESCRIPTORS: [c_int; PORT_COUNT] = [
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
    CONTROL_IN,
    CONTROL_IN,
];

pub(crate) struct SyncPtrs<T>(pub(crate) T);
// SAFETY: the wrapped data is immutable static C strings / constants.
unsafe impl<T> Sync for SyncPtrs<T> {}

static PORT_NAMES: SyncPtrs<[*const c_char; PORT_COUNT]> = SyncPtrs([
    c"In 0".as_ptr(),
    c"In 1".as_ptr(),
    c"In 2".as_ptr(),
    c"In 3".as_ptr(),
    c"In 4".as_ptr(),
    c"In 5".as_ptr(),
    c"In 6".as_ptr(),
    c"Out".as_ptr(),
    c"Gain dB".as_ptr(),
    c"Mic mask".as_ptr(),
    c"Beams".as_ptr(),
    c"Calibrate".as_ptr(),
    c"Array".as_ptr(),
]);

pub(crate) const fn hint(
    hint_descriptor: c_int,
    lower_bound: f32,
    upper_bound: f32,
) -> PortRangeHint {
    PortRangeHint {
        hint_descriptor,
        lower_bound,
        upper_bound,
    }
}

static PORT_HINTS: [PortRangeHint; PORT_COUNT] = [
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
    hint(
        BOUNDED | HINT_INTEGER | HINT_DEFAULT_MAXIMUM,
        1.0,
        MAX_BEAMS as f32,
    ),
    hint(HINT_TOGGLED | HINT_DEFAULT_1, 0.0, 1.0),
    hint(
        BOUNDED | HINT_INTEGER | HINT_DEFAULT_0,
        ARRAY_RING as f32,
        ARRAY_ECHO_DOT_5 as f32,
    ),
];

static DESCRIPTOR: SyncPtrs<Descriptor> = SyncPtrs(Descriptor {
    unique_id: UNIQUE_ID as c_ulong,
    label: c"pinna_beam".as_ptr(),
    properties: PROPERTY_HARD_RT_CAPABLE,
    name: c"Pinna Echo Dot beamformer".as_ptr(),
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

/// Standard LADSPA entry point: index 0 is `pinna_beam`, 1 `pinna_wake`.
#[unsafe(no_mangle)]
pub extern "C" fn ladspa_descriptor(index: c_ulong) -> *const Descriptor {
    match index {
        0 => &DESCRIPTOR.0,
        1 => &crate::wake::DESCRIPTOR.0,
        _ => ptr::null(),
    }
}

struct Plugin {
    ports: [*mut Data; PORT_COUNT],
    calibration: Calibration,
    beamformer: Box<Beamformer>,
    /// The lanes the beamformer reads; the others may stay unlinked.
    mask: u8,
    highpass: HighPass,
    gain_db: f32,
    gain_hundredths: u32,
    /// Beam snapshot for pinnad's listening ring; `None` where the file
    /// cannot be created (PipeWire outside the device).
    view: Option<BeamViewWriter>,
}

/// `100 * 10^(db / 20)`, rounded and clamped to the integer gain stage's range.
pub(crate) fn gain_hundredths(db: f32) -> u32 {
    let g = 100.0 * 10f32.powf(db / 20.0);
    if g.is_nan() {
        return DIGITAL_GAIN_UNITY;
    }
    (g.round() as u32).clamp(DIGITAL_GAIN_UNITY, DIGITAL_GAIN_MAX)
}

/// Float control to integer, clamped to `lo..=hi`; NaN gives `lo`.
pub(crate) fn control_int(v: f32, lo: usize, hi: usize) -> usize {
    (v.round() as usize).clamp(lo, hi)
}

/// Round and saturate a float sample in [-1, 1] to the i16 scale.
pub(crate) fn to_i16(x: f32) -> i16 {
    saturate_s16((x * 32768.0).round() as i64)
}

/// The beamformer for `array`. The ring is the plain constructor's; the L's
/// positions come from the board description.
fn design(
    array: usize,
    mask: u8,
    beams: usize,
) -> Result<Beamformer, pinna_audio::beamformer::BeamError> {
    if array == ARRAY_ECHO_DOT_5 {
        let mics = &pinna_board::ECHO_DOT_5.mics;
        let positions: Vec<(f64, f64)> =
            (0..mics.count).map(|m| mics.geometry.position(m)).collect();
        Beamformer::with_positions(&positions, mask, beams)
    } else {
        Beamformer::new(mask, beams)
    }
}

unsafe extern "C" fn instantiate(_d: *const Descriptor, rate: c_ulong) -> Handle {
    if rate != c_ulong::from(SAMPLE_RATE) {
        return ptr::null_mut();
    }
    catch_unwind(|| {
        let plugin = Box::new(Plugin {
            ports: [ptr::null_mut(); PORT_COUNT],
            calibration: Calibration::new(pinna_hal::ident::read_miccal()),
            beamformer: Box::new(Beamformer::default_echo_dot()),
            mask: DEFAULT_MIC_MASK,
            highpass: HighPass::new(),
            gain_db: DEFAULT_GAIN_DB,
            gain_hundredths: gain_hundredths(DEFAULT_GAIN_DB),
            view: BeamViewWriter::create(Path::new(beamview::PATH)).ok(),
        });
        Box::into_raw(plugin).cast()
    })
    .unwrap_or(ptr::null_mut())
}

unsafe extern "C" fn connect_port(h: Handle, port: c_ulong, data: *mut Data) {
    let p = h.cast::<Plugin>();
    if p.is_null() {
        return;
    }
    // SAFETY: `h` came from `instantiate`; the host serialises calls on it.
    if let Some(slot) = unsafe { (*p).ports.get_mut(port as usize) } {
        *slot = data;
    }
}

unsafe extern "C" fn activate(h: Handle) {
    let p = h.cast::<Plugin>();
    if p.is_null() {
        return;
    }
    let _ = catch_unwind(|| {
        // SAFETY: `h` came from `instantiate`; the host serialises calls on it.
        let p = unsafe { &mut *p };
        let read = |port: usize| {
            let ptr = p.ports[port];
            // SAFETY: control ports are connected by the host before activate.
            (!ptr.is_null()).then(|| unsafe { *ptr })
        };
        let mask = read(PORT_MASK).map_or(usize::from(DEFAULT_MIC_MASK), |v| {
            control_int(v, 1, usize::from(MIC_MASK_ALL))
        });
        let beams = read(PORT_BEAMS).map_or(DEFAULT_BEAMS, |v| control_int(v, 1, MAX_BEAMS));
        let array =
            read(PORT_ARRAY).map_or(ARRAY_RING, |v| control_int(v, ARRAY_RING, ARRAY_ECHO_DOT_5));
        // An invalid combination (e.g. fewer than two mics) keeps the old design.
        if let Ok(bf) = design(array, mask as u8, beams) {
            *p.beamformer = bf;
            p.mask = mask as u8;
        }
        p.highpass = HighPass::new();
    });
}

unsafe extern "C" fn run(h: Handle, n: c_ulong) {
    let p = h.cast::<Plugin>();
    if p.is_null() {
        return;
    }
    // SAFETY: `h` came from `instantiate`; the host serialises calls on it.
    let p = unsafe { &mut *p };
    // Unlinked inputs of lanes outside the mask may be null, and Array is read
    // only on activate; nothing reads them in a run.
    if p.ports.iter().enumerate().any(|(port, q)| {
        q.is_null() && !(port < LOGICAL_CHANNELS && p.mask & (1 << port) == 0) && port != PORT_ARRAY
    }) {
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
        let mut logical = [0i16; LOGICAL_CHANNELS];
        for (c, l) in logical.iter_mut().enumerate() {
            if p.mask & (1 << c) == 0 {
                continue;
            }
            // SAFETY: the host guarantees `n` valid samples on connected audio
            // ports; raw reads keep an in-place host (out aliasing an input) sound.
            let s = to_i16(unsafe { *p.ports[c].add(i) });
            *l = if calibrate {
                p.calibration.apply(c, s)
            } else {
                s
            };
        }
        let mut y = [p.highpass.sample(p.beamformer.sample(&logical))];
        apply_digital_gain(&mut y, p.gain_hundredths);
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
        let _ = catch_unwind(|| drop(unsafe { Box::from_raw(h.cast::<Plugin>()) }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pinna_audio::dsp::DIGITAL_GAIN_DEFAULT;

    /// One beam plugin run over `frames` frames of lanes 0..2 carrying a
    /// 1 kHz tone; lanes 3..6 are `None` (unlinked) or silent buffers.
    fn run_beam(array: f32, mask: f32, linked_silence: bool) -> Vec<f32> {
        const N: usize = 160;
        let tone: Vec<f32> = (0..N)
            .map(|i| 0.1 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 16000.0).sin())
            .collect();
        let mut silence = vec![0f32; N];
        let mut ins: Vec<Vec<f32>> = (0..3).map(|_| tone.clone()).collect();
        let mut out = vec![0f32; N];
        let (mut gain, mut mask, mut beams, mut calibrate, mut array) =
            (0f32, mask, 12f32, 0f32, array);
        // SAFETY: the buffers outlive the handle and hold `N` samples each.
        unsafe {
            let h = instantiate(&DESCRIPTOR.0, c_ulong::from(SAMPLE_RATE));
            assert!(!h.is_null());
            for (c, buf) in ins.iter_mut().enumerate() {
                connect_port(h, c as c_ulong, buf.as_mut_ptr());
            }
            if linked_silence {
                for c in 3..LOGICAL_CHANNELS {
                    connect_port(h, c as c_ulong, silence.as_mut_ptr());
                }
            }
            connect_port(h, PORT_OUT as c_ulong, out.as_mut_ptr());
            connect_port(h, PORT_GAIN as c_ulong, &mut gain);
            connect_port(h, PORT_MASK as c_ulong, &mut mask);
            connect_port(h, PORT_BEAMS as c_ulong, &mut beams);
            connect_port(h, PORT_CALIBRATE as c_ulong, &mut calibrate);
            connect_port(h, PORT_ARRAY as c_ulong, &mut array);
            activate(h);
            for _ in 0..8 {
                run(h, N as c_ulong);
            }
            cleanup(h);
        }
        out
    }

    #[test]
    fn the_array_control_picks_the_l_and_a_three_microphone_mask_runs_unlinked() {
        let linked = run_beam(1.0, 7.0, true);
        let unlinked = run_beam(1.0, 7.0, false);
        assert_eq!(linked, unlinked);
        assert!(linked.iter().any(|&v| v != 0.0));
        // The ring's mask 7 puts lanes 0..2 in its own geometry: a different design.
        assert_ne!(linked, run_beam(0.0, 7.0, true));
    }

    #[test]
    fn default_gain_is_pinnad_default() {
        assert_eq!(gain_hundredths(DEFAULT_GAIN_DB), DIGITAL_GAIN_DEFAULT);
        assert_eq!(gain_hundredths(0.0), DIGITAL_GAIN_UNITY);
        assert_eq!(gain_hundredths(30.0), DIGITAL_GAIN_MAX);
        assert_eq!(gain_hundredths(f32::NAN), DIGITAL_GAIN_UNITY);
    }
}
