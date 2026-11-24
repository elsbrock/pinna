//! Reading and writing recordings. Reads PCM of 16, 24 or 32 bits and 32-bit
//! float, plain or WAVE_FORMAT_EXTENSIBLE: pw-record's files and the
//! dashboard's debugging file (ADR 0030) alike. Samples come back at full
//! scale 1.0.

use std::path::Path;

use anyhow::{Context, Result, ensure};

pub struct Wav {
    pub rate: u32,
    pub channels: usize,
    /// Interleaved.
    pub samples: Vec<f32>,
}

impl Wav {
    pub fn channel(&self, c: usize) -> Vec<f32> {
        self.samples
            .iter()
            .skip(c)
            .step_by(self.channels)
            .copied()
            .collect()
    }
}

pub fn read(path: &Path) -> Result<Wav> {
    let mut r =
        hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = r.spec();
    let channels = usize::from(spec.channels);
    ensure!(channels > 0, "{}: no channels", path.display());
    let mut samples = Vec::with_capacity(r.len() as usize);
    // pw-record may stop mid-frame; keep what was read before an error.
    match spec.sample_format {
        hound::SampleFormat::Float => {
            ensure!(
                spec.bits_per_sample == 32,
                "{}: a float WAV must be 32-bit",
                path.display()
            );
            for s in r.samples::<f32>() {
                match s {
                    Ok(v) => samples.push(v),
                    Err(_) => break,
                }
            }
        }
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f64;
            for s in r.samples::<i32>() {
                match s {
                    Ok(v) => samples.push((f64::from(v) * scale) as f32),
                    Err(_) => break,
                }
            }
        }
    }
    samples.truncate(samples.len() / channels * channels);
    Ok(Wav {
        rate: spec.sample_rate,
        channels,
        samples,
    })
}

/// 32-bit float, which `tools/aec-replay` reads.
pub fn write(path: &Path, rate: u32, channels: usize, interleaved: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: channels as u16,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)
        .with_context(|| format!("creating {}", path.display()))?;
    for &x in interleaved {
        w.write_sample(x)?;
    }
    w.finalize()?;
    Ok(())
}
