#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub const KEYWORD: &str = "alexa_v0.1.onnx";

pub fn model_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../blobs/models/openwakeword-0.5.1");
    if dir.join(KEYWORD).exists() {
        Some(dir)
    } else {
        eprintln!(
            "SKIP: models missing in {}; run `just models`",
            dir.display()
        );
        None
    }
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn read_wav(path: &Path) -> Vec<i16> {
    let mut reader = hound::WavReader::open(path).unwrap();
    let spec = reader.spec();
    assert_eq!(
        (spec.channels, spec.sample_rate, spec.bits_per_sample),
        (1, 16000, 16)
    );
    reader.samples::<i16>().map(Result::unwrap).collect()
}
