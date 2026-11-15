//! Run the Echo Dot microphone front end (as in the LADSPA plugin, unity
//! calibration) over a 9-lane 16 kHz WAV and write the mono result.
//!
//!   cargo run -p pinna-audio --example beam_wav -- in.wav out.wav [lane-order e.g. 0,1,2,3,4,5,6]

use pinna_audio::beamformer::Beamformer;
use pinna_audio::dsp::{HighPass, LOGICAL_CHANNELS, apply_digital_gain};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let order: Vec<usize> = args
        .get(3)
        .map(|s| s.split(',').map(|x| x.parse().unwrap()).collect())
        .unwrap_or_else(|| (0..LOGICAL_CHANNELS).collect());
    let mut r = hound::WavReader::open(&args[1]).expect("input");
    let ch = usize::from(r.spec().channels);
    let samples: Vec<i16> = r.samples::<i16>().map(|s| s.unwrap()).collect();
    let mut bf = Beamformer::default_echo_dot();
    let mut hp = HighPass::new();
    let mut out = Vec::with_capacity(samples.len() / ch);
    for frame in samples.chunks_exact(ch) {
        let mut l = [0i16; LOGICAL_CHANNELS];
        for (i, o) in order.iter().enumerate() {
            l[i] = frame[*o];
        }
        out.push(hp.sample(bf.sample(&l)));
    }
    apply_digital_gain(&mut out, 400);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&args[2], spec).expect("output");
    for s in out {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
}
