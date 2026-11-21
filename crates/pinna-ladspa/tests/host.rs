//! Drives the plugin through the descriptor like a LADSPA host would.

use pinna_audio::beamformer::Beamformer;
use pinna_audio::dsp::{
    Calibration, DIGITAL_GAIN_DEFAULT, HighPass, LOGICAL_CHANNELS, apply_digital_gain,
};
use pinna_ladspa::ladspa::*;
use pinna_ladspa::{LABEL, UNIQUE_ID, ladspa_descriptor};
use std::ffi::CStr;
use std::os::raw::c_ulong;

fn descriptor() -> &'static Descriptor {
    let d = ladspa_descriptor(0);
    assert!(!d.is_null());
    assert!(ladspa_descriptor(2).is_null());
    unsafe { &*d }
}

struct Controls {
    gain_db: f32,
    mask: f32,
    beams: f32,
    calibrate: f32,
}

impl Default for Controls {
    fn default() -> Self {
        Controls {
            gain_db: 12.0412,
            mask: 27.0,
            beams: 12.0,
            calibrate: 1.0,
        }
    }
}

struct Host {
    d: &'static Descriptor,
    h: Handle,
    inputs: [Vec<f32>; LOGICAL_CHANNELS],
    out: Vec<f32>,
    c: Box<Controls>,
}

impl Host {
    fn new(c: Controls, len: usize) -> Host {
        let d = descriptor();
        let h = unsafe { (d.instantiate.unwrap())(d, 16000) };
        assert!(!h.is_null());
        let mut host = Host {
            d,
            h,
            inputs: std::array::from_fn(|_| vec![0.0; len]),
            out: vec![0.0; len],
            c: Box::new(c),
        };
        host.connect();
        host
    }

    fn connect(&mut self) {
        let cp = self.d.connect_port.unwrap();
        for (i, v) in self.inputs.iter_mut().enumerate() {
            unsafe { cp(self.h, i as c_ulong, v.as_mut_ptr()) };
        }
        unsafe {
            cp(self.h, 7, self.out.as_mut_ptr());
            cp(self.h, 8, &mut self.c.gain_db);
            cp(self.h, 9, &mut self.c.mask);
            cp(self.h, 10, &mut self.c.beams);
            cp(self.h, 11, &mut self.c.calibrate);
        }
    }

    fn activate(&self) {
        unsafe { (self.d.activate.unwrap())(self.h) };
    }

    fn run(&self, n: usize) {
        unsafe { (self.d.run.unwrap())(self.h, n as c_ulong) };
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        unsafe { (self.d.cleanup.unwrap())(self.h) };
    }
}

/// A plane wave: the same tone with a small per-lane delay, plus a bit of noise.
fn plane_wave(len: usize) -> [Vec<i16>; LOGICAL_CHANNELS] {
    let mut seed = 12345u32;
    std::array::from_fn(|c| {
        (0..len)
            .map(|n| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = ((seed >> 16) % 200) as f64 - 100.0;
                let t = n as f64 - 0.7 * c as f64;
                (6000.0 * (2.0 * std::f64::consts::PI * 1000.0 * t / 16000.0).sin()
                    + 2000.0 * (2.0 * std::f64::consts::PI * 350.0 * t / 16000.0).sin()
                    + noise) as i16
            })
            .collect()
    })
}

fn reference(inputs: &[Vec<i16>; LOGICAL_CHANNELS], mask: u8, beams: usize, gain: u32) -> Vec<f32> {
    let cal = Calibration::unity();
    let mut bf = Beamformer::new(mask, beams).unwrap();
    let mut hp = HighPass::new();
    (0..inputs[0].len())
        .map(|n| {
            let mut l = [0i16; LOGICAL_CHANNELS];
            for (c, v) in l.iter_mut().enumerate() {
                *v = cal.apply(c, inputs[c][n]);
            }
            let mut y = [hp.sample(bf.sample(&l))];
            apply_digital_gain(&mut y, gain);
            f32::from(y[0]) / 32768.0
        })
        .collect()
}

fn feed(host: &mut Host, wave: &[Vec<i16>; LOGICAL_CHANNELS]) {
    for (dst, src) in host.inputs.iter_mut().zip(wave) {
        for (d, &s) in dst.iter_mut().zip(src) {
            *d = f32::from(s) / 32768.0;
        }
    }
}

#[test]
fn descriptor_metadata() {
    let d = descriptor();
    let s = |p| unsafe { CStr::from_ptr(p) }.to_str().unwrap();
    assert_eq!(d.unique_id as u32, UNIQUE_ID);
    assert_eq!(s(d.label), LABEL);
    assert_eq!(s(d.label), "pinna_beam");
    assert_eq!(s(d.name), "Pinna Echo Dot beamformer");
    assert_eq!(s(d.maker), "Pinna");
    assert_eq!(s(d.copyright), "MIT");
    assert_eq!(d.port_count, 12);
    let desc = unsafe { std::slice::from_raw_parts(d.port_descriptors, 12) };
    let names = unsafe { std::slice::from_raw_parts(d.port_names, 12) };
    let hints = unsafe { std::slice::from_raw_parts(d.port_range_hints, 12) };
    for i in 0..7 {
        assert_eq!(s(names[i]), format!("In {i}"));
        assert_eq!(desc[i], PORT_INPUT | PORT_AUDIO);
    }
    assert_eq!(s(names[7]), "Out");
    assert_eq!(desc[7], PORT_OUTPUT | PORT_AUDIO);
    for (i, n) in ["Gain dB", "Mic mask", "Beams", "Calibrate"]
        .iter()
        .enumerate()
    {
        assert_eq!(s(names[8 + i]), *n);
        assert_eq!(desc[8 + i], PORT_INPUT | PORT_CONTROL);
    }
    let bounded = HINT_BOUNDED_BELOW | HINT_BOUNDED_ABOVE;
    assert_eq!((hints[8].lower_bound, hints[8].upper_bound), (0.0, 30.0));
    assert_eq!(hints[8].hint_descriptor & bounded, bounded);
    assert_eq!((hints[9].lower_bound, hints[9].upper_bound), (1.0, 127.0));
    assert_ne!(hints[9].hint_descriptor & HINT_INTEGER, 0);
    assert_eq!((hints[10].lower_bound, hints[10].upper_bound), (1.0, 12.0));
    assert_ne!(hints[10].hint_descriptor & HINT_INTEGER, 0);
    assert_eq!(
        hints[10].hint_descriptor & HINT_DEFAULT_MASK,
        HINT_DEFAULT_MAXIMUM
    );
    assert_ne!(hints[11].hint_descriptor & HINT_TOGGLED, 0);
    assert_eq!(
        hints[11].hint_descriptor & HINT_DEFAULT_MASK,
        HINT_DEFAULT_1
    );
}

#[test]
fn default_constants_match_pinnad() {
    assert_eq!(pinna_ladspa::DEFAULT_MIC_MASK, 27);
    assert_eq!(pinna_ladspa::DEFAULT_BEAMS, 12);
    let hundredths = 100.0 * 10f32.powf(pinna_ladspa::DEFAULT_GAIN_DB / 20.0);
    assert_eq!(hundredths.round() as u32, DIGITAL_GAIN_DEFAULT);
}

#[test]
fn rejects_other_sample_rates() {
    let d = descriptor();
    for rate in [48000, 44100, 8000, 0] {
        assert!(unsafe { (d.instantiate.unwrap())(d, rate) }.is_null());
    }
}

#[test]
fn matches_pure_rust_chain() {
    let len = 4096;
    let wave = plane_wave(len);
    let mut host = Host::new(Controls::default(), len);
    feed(&mut host, &wave);
    host.activate();
    host.run(len);
    let want = reference(&wave, 27, 12, DIGITAL_GAIN_DEFAULT);
    assert_eq!(host.out, want);
    assert!(host.out.iter().any(|&v| v != 0.0));
}

#[test]
fn run_in_chunks_equals_one_shot() {
    let len = 2048;
    let wave = plane_wave(len);
    let mut host = Host::new(Controls::default(), len);
    feed(&mut host, &wave);
    host.activate();
    // Same buffers, split into two calls through shifted pointers.
    let half = len / 2;
    host.run(half);
    let first = host.out[..half].to_vec();
    let cp = host.d.connect_port.unwrap();
    for (i, v) in host.inputs.iter_mut().enumerate() {
        unsafe { cp(host.h, i as c_ulong, v.as_mut_ptr().add(half)) };
    }
    unsafe { cp(host.h, 7, host.out.as_mut_ptr().add(half)) };
    host.run(half);
    let mut got = first;
    got.extend_from_slice(&host.out[half..]);
    assert_eq!(got, reference(&wave, 27, 12, DIGITAL_GAIN_DEFAULT)[..len]);
}

#[test]
fn mask_and_beams_apply_on_reactivate() {
    let len = 2048;
    let wave = plane_wave(len);
    let mut host = Host::new(Controls::default(), len);
    feed(&mut host, &wave);
    host.activate();
    host.run(len);
    assert_eq!(host.out, reference(&wave, 27, 12, DIGITAL_GAIN_DEFAULT));

    host.c.mask = 63.0;
    host.c.beams = 6.0;
    // Not applied until activate.
    host.activate();
    host.run(len);
    let want = reference(&wave, 63, 6, DIGITAL_GAIN_DEFAULT);
    assert_eq!(host.out, want);
    assert_ne!(want, reference(&wave, 27, 12, DIGITAL_GAIN_DEFAULT));
}

#[test]
fn gain_is_live() {
    let len = 2048;
    let wave = plane_wave(len);
    let mut host = Host::new(
        Controls {
            gain_db: 0.0,
            ..Controls::default()
        },
        len,
    );
    feed(&mut host, &wave);
    host.activate();
    host.run(len);
    assert_eq!(host.out, reference(&wave, 27, 12, 100));
}

#[test]
fn out_of_range_floats_saturate() {
    let mut host = Host::new(Controls::default(), 512);
    for v in &mut host.inputs {
        v.fill(4.0);
    }
    host.inputs[0][3] = f32::NAN;
    host.activate();
    host.run(512);
    assert!(host.out.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
}
