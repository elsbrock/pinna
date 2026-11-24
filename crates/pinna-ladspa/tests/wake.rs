//! Drives `pinna_wake` through its descriptor like a LADSPA host would.

use pinna_audio::dsp::{
    Calibration, DIGITAL_GAIN_DEFAULT, HighPass, LOGICAL_CHANNELS, apply_digital_gain, saturate_s16,
};
use pinna_audio::echocancel::LinearEchoCanceller;
use pinna_ladspa::ladspa::*;
use pinna_ladspa::ladspa_descriptor;
use pinna_ladspa::wake::{INPUTS, LABEL, UNIQUE_ID};
use std::ffi::CStr;
use std::os::raw::c_ulong;

/// The Echo Dot (2nd gen)'s board, which hardware Pinna doesn't know gets:
/// the speaker plays the right channel, loopback lane 8.
const SPEAKER_LANE: usize = 8;
const OTHER_LOOPBACK: usize = 7;

fn descriptor() -> &'static Descriptor {
    let d = ladspa_descriptor(1);
    assert!(!d.is_null());
    unsafe { &*d }
}

struct Host {
    d: &'static Descriptor,
    h: Handle,
    inputs: [Vec<f32>; INPUTS],
    out: Vec<f32>,
    controls: Box<[f32; 3]>,
}

impl Host {
    /// Gain dB, mic mask, calibrate.
    fn new(controls: [f32; 3], len: usize) -> Host {
        let d = descriptor();
        let h = unsafe { (d.instantiate.unwrap())(d, 16000) };
        assert!(!h.is_null());
        let mut host = Host {
            d,
            h,
            inputs: std::array::from_fn(|_| vec![0.0; len]),
            out: vec![0.0; len],
            controls: Box::new(controls),
        };
        let cp = d.connect_port.unwrap();
        for (i, v) in host.inputs.iter_mut().enumerate() {
            unsafe { cp(h, i as c_ulong, v.as_mut_ptr()) };
        }
        unsafe {
            cp(h, INPUTS as c_ulong, host.out.as_mut_ptr());
            for (i, c) in host.controls.iter_mut().enumerate() {
                cp(h, (INPUTS + 1 + i) as c_ulong, c);
            }
            (d.activate.unwrap())(h);
        }
        host
    }

    fn run(&self) {
        unsafe { (self.d.run.unwrap())(self.h, self.out.len() as c_ulong) };
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        unsafe { (self.d.cleanup.unwrap())(self.h) };
    }
}

/// White noise on the 16-bit scale.
fn noise(seed: &mut u32, amp: f32) -> f32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    ((*seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 2.0 * amp
}

/// Playback on the speaker's loopback lane and its echo at every microphone:
/// a delay of about 2 ms and a reflection, different per lane. The other
/// loopback lane carries a channel the speaker doesn't play.
fn playback(len: usize) -> [Vec<f32>; INPUTS] {
    let mut seed = 99;
    let reference: Vec<f32> = (0..len).map(|_| noise(&mut seed, 6000.0)).collect();
    let unheard: Vec<f32> = (0..len).map(|_| noise(&mut seed, 6000.0)).collect();
    let mut lanes: [Vec<f32>; INPUTS] = std::array::from_fn(|c| {
        if c == SPEAKER_LANE {
            return reference.clone();
        }
        if c == OTHER_LOOPBACK {
            return unheard.clone();
        }
        let (d1, d2) = (33 + c, 90 + 7 * c);
        (0..len)
            .map(|n| {
                let at = |d: usize| if n >= d { reference[n - d] } else { 0.0 };
                0.4 * at(d1) - 0.15 * at(d2)
            })
            .collect()
    });
    for v in lanes.iter_mut() {
        for x in v.iter_mut() {
            *x = (*x).round() / 32768.0;
        }
    }
    lanes
}

fn db(v: &[f32]) -> f32 {
    10.0 * (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32 + 1e-12).log10()
}

#[test]
fn descriptor_metadata() {
    let d = descriptor();
    assert_eq!(d.unique_id, UNIQUE_ID as c_ulong);
    assert_eq!(unsafe { CStr::from_ptr(d.label) }.to_str().unwrap(), LABEL);
    assert_eq!(d.port_count, 13);
    let name = |i: usize| {
        unsafe { CStr::from_ptr(*d.port_names.add(i)) }
            .to_str()
            .unwrap()
    };
    assert_eq!(name(8), "In 8");
    assert_eq!(name(9), "Out");
    assert_eq!(name(11), "Mic mask");
    assert!(ladspa_descriptor(2).is_null());
}

#[test]
fn matches_the_pure_rust_chain() {
    let len = 4096;
    let mut host = Host::new([12.0412, 127.0, 1.0], len);
    host.inputs = playback(len);
    // The connected buffers moved; connect them again.
    let cp = host.d.connect_port.unwrap();
    for (i, v) in host.inputs.iter_mut().enumerate() {
        unsafe { cp(host.h, i as c_ulong, v.as_mut_ptr()) };
    }
    host.run();

    let cal = Calibration::unity();
    let mut aec = LinearEchoCanceller::new();
    let mut hp = HighPass::new();
    let i16_of = |x: f32| (x * 32768.0).round() as i16;
    let want: Vec<f32> = (0..len)
        .map(|n| {
            let mic = (0..LOGICAL_CHANNELS)
                .map(|c| f32::from(cal.apply(c, i16_of(host.inputs[c][n]))))
                .sum::<f32>()
                / LOGICAL_CHANNELS as f32;
            let e = aec.process(mic, f32::from(i16_of(host.inputs[SPEAKER_LANE][n])));
            let mut y = [hp.sample(saturate_s16(e.round() as i64))];
            apply_digital_gain(&mut y, DIGITAL_GAIN_DEFAULT);
            f32::from(y[0]) / 32768.0
        })
        .collect();
    assert_eq!(host.out, want);
}

#[test]
fn it_cancels_the_echo_of_the_speakers_loopback_lane() {
    // The canceller's step gains about 3 dB a second on noise (ADR 0033).
    let len = 10 * 16_000;
    let lanes = playback(len);
    let run = |with_reference: bool| {
        let mut host = Host::new([12.0412, 127.0, 1.0], len);
        host.inputs = lanes.clone();
        if !with_reference {
            host.inputs[SPEAKER_LANE].fill(0.0);
        }
        let cp = host.d.connect_port.unwrap();
        for (i, v) in host.inputs.iter_mut().enumerate() {
            unsafe { cp(host.h, i as c_ulong, v.as_mut_ptr()) };
        }
        host.run();
        db(&host.out[9 * 16_000..])
    };
    let removed = run(false) - run(true);
    assert!(removed > 25.0, "removed {removed:.1} dB");
}

/// The loopback lane of the channel the speaker doesn't play is no reference:
/// with only it, nothing is cancelled.
#[test]
fn the_other_loopback_lane_is_ignored() {
    let len = 16_000;
    let lanes = playback(len);
    let run = |swap: bool| {
        let mut host = Host::new([12.0412, 127.0, 1.0], len);
        host.inputs = lanes.clone();
        if swap {
            host.inputs[OTHER_LOOPBACK] = vec![0.0; len];
        }
        let cp = host.d.connect_port.unwrap();
        for (i, v) in host.inputs.iter_mut().enumerate() {
            unsafe { cp(host.h, i as c_ulong, v.as_mut_ptr()) };
        }
        host.run();
        host.out.clone()
    };
    assert_eq!(run(false), run(true));
}
