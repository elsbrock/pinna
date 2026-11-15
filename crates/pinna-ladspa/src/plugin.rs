use crate::ladspa::*;
use pinna_audio::beamformer::{Beamformer, DEFAULT_BEAMS as BF_DEFAULT_BEAMS, MAX_BEAMS};
use pinna_audio::dsp::{
    Calibration, DIGITAL_GAIN_MAX, DIGITAL_GAIN_UNITY, HighPass, LOGICAL_CHANNELS, MIC_MASK_ALL,
    MIC_MASK_BEAM, apply_digital_gain, saturate_s16,
};
use std::os::raw::{c_char, c_int, c_ulong};
use std::panic::catch_unwind;
use std::ptr;

/// The only supported sample rate.
pub const SAMPLE_RATE: u32 = 16_000;
/// LADSPA unique ID. 0x50494E ("PIN") sits in the unofficial range: IDs
/// 1-1000 are reserved for private/testing use and nothing registered with
/// ladspa.org uses this one, so it is fixed here and never changes.
pub const UNIQUE_ID: u32 = 0x0050_494E;
pub const LABEL: &str = "pinna_beam";
/// 20 * log10(4): the x4 (400 hundredths) digital gain pinnad uses.
pub const DEFAULT_GAIN_DB: f32 = 12.041_2;
pub const DEFAULT_MIC_MASK: u8 = MIC_MASK_BEAM;
pub const DEFAULT_BEAMS: usize = BF_DEFAULT_BEAMS;

const PORT_GAIN: usize = LOGICAL_CHANNELS + 1;
const PORT_MASK: usize = PORT_GAIN + 1;
const PORT_BEAMS: usize = PORT_MASK + 1;
const PORT_CALIBRATE: usize = PORT_BEAMS + 1;
const PORT_COUNT: usize = PORT_CALIBRATE + 1;
const PORT_OUT: usize = LOGICAL_CHANNELS;

const AUDIO_IN: c_int = PORT_INPUT | PORT_AUDIO;
const CONTROL_IN: c_int = PORT_INPUT | PORT_CONTROL;
const BOUNDED: c_int = HINT_BOUNDED_BELOW | HINT_BOUNDED_ABOVE;

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
];

struct SyncPtrs<T>(T);
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
]);

const fn hint(hint_descriptor: c_int, lower_bound: f32, upper_bound: f32) -> PortRangeHint {
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

/// Standard LADSPA entry point; returns a pointer to a static descriptor.
#[unsafe(no_mangle)]
pub extern "C" fn ladspa_descriptor(index: c_ulong) -> *const Descriptor {
    if index == 0 {
        &DESCRIPTOR.0
    } else {
        ptr::null()
    }
}

struct Plugin {
    ports: [*mut Data; PORT_COUNT],
    calibration: Calibration,
    beamformer: Box<Beamformer>,
    highpass: HighPass,
    gain_db: f32,
    gain_hundredths: u32,
}

/// `100 * 10^(db / 20)`, rounded and clamped to the integer gain stage's range.
fn gain_hundredths(db: f32) -> u32 {
    let g = 100.0 * 10f32.powf(db / 20.0);
    if g.is_nan() {
        return DIGITAL_GAIN_UNITY;
    }
    (g.round() as u32).clamp(DIGITAL_GAIN_UNITY, DIGITAL_GAIN_MAX)
}

/// Float control to integer, clamped to `lo..=hi`; NaN gives `lo`.
fn control_int(v: f32, lo: usize, hi: usize) -> usize {
    (v.round() as usize).clamp(lo, hi)
}

/// Round and saturate a float sample in [-1, 1] to the i16 scale.
fn to_i16(x: f32) -> i16 {
    saturate_s16((x * 32768.0).round() as i64)
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
            highpass: HighPass::new(),
            gain_db: DEFAULT_GAIN_DB,
            gain_hundredths: gain_hundredths(DEFAULT_GAIN_DB),
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
        // An invalid combination (e.g. fewer than two mics) keeps the old design.
        if let Ok(bf) = Beamformer::new(mask as u8, beams) {
            *p.beamformer = bf;
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
    if p.ports.iter().any(|q| q.is_null()) {
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

    #[test]
    fn default_gain_is_pinnad_default() {
        assert_eq!(gain_hundredths(DEFAULT_GAIN_DB), DIGITAL_GAIN_DEFAULT);
        assert_eq!(gain_hundredths(0.0), DIGITAL_GAIN_UNITY);
        assert_eq!(gain_hundredths(30.0), DIGITAL_GAIN_MAX);
        assert_eq!(gain_hundredths(f32::NAN), DIGITAL_GAIN_UNITY);
    }
}
