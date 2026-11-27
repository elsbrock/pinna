//! A voiceprint: the L2-normalised embedding of 1.5 s of speech (ADR 0038).

use crate::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct Voiceprint(Vec<f32>);

impl Voiceprint {
    /// Normalises `v`. `None` when it is empty, has a non-finite value or has no length.
    pub(crate) fn from_raw(mut v: Vec<f32>) -> Option<Self> {
        if v.is_empty() || v.iter().any(|x| !x.is_finite()) {
            return None;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm == 0.0 || !norm.is_finite() {
            return None;
        }
        v.iter_mut().for_each(|x| *x /= norm);
        Some(Self(v))
    }

    pub fn dim(&self) -> usize {
        self.0.len()
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    /// Both are normalised, so the dot product is the cosine. Voiceprints of different sizes
    /// compare as 0.
    pub fn cosine(&self, other: &Voiceprint) -> f32 {
        if self.0.len() != other.0.len() {
            return 0.0;
        }
        self.0.iter().zip(&other.0).map(|(a, b)| a * b).sum()
    }

    /// The normalised mean. `None` for no voiceprints, differing sizes, or a mean of zero length.
    pub fn mean(all: &[Voiceprint]) -> Option<Voiceprint> {
        let first = all.first()?;
        if all.iter().any(|v| v.dim() != first.dim()) {
            return None;
        }
        let mut sum = vec![0f32; first.dim()];
        for v in all {
            sum.iter_mut().zip(&v.0).for_each(|(s, x)| *s += x);
        }
        Voiceprint::from_raw(sum)
    }

    pub fn to_le_bytes(&self) -> Vec<u8> {
        self.0.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    pub fn from_le_bytes(bytes: &[u8]) -> Result<Voiceprint, Error> {
        if bytes.is_empty() {
            return Err(Error::Bytes("there are none; enroll the person again"));
        }
        if !bytes.len().is_multiple_of(4) {
            return Err(Error::Bytes(
                "the length isn't a multiple of 4; enroll the person again",
            ));
        }
        let v: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        if v.iter().any(|x| !x.is_finite()) {
            return Err(Error::Bytes(
                "a value isn't a finite number; enroll the person again",
            ));
        }
        // Stored voiceprints are normalised already; renormalising keeps the dot product a cosine.
        Voiceprint::from_raw(v).ok_or(Error::Bytes("all values are zero; enroll the person again"))
    }
}
