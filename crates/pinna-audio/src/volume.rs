//! Playback volume curve, ported from LibreEcho's audio_engine.c.

/// Raw codec-equivalent index for a logical volume 1..=100:
/// `103 - round(96 * (1 - x)^1.3)` with `x = (p - 1) / 99`.
pub fn logical_master_raw(percent: u8) -> i32 {
    let p = f64::from(percent.clamp(1, 100));
    let x = (p - 1.0) / 99.0;
    103 - (96.0 * (1.0 - x).powf(1.3) + 0.5).floor() as i32
}

/// Gain in dB for a logical volume (`(raw - 127) / 2`). `None` for 0 (mute).
pub fn volume_db(percent: u8) -> Option<f64> {
    if percent == 0 {
        return None;
    }
    Some((f64::from(logical_master_raw(percent)) - 127.0) / 2.0)
}

/// Linear amplitude gain for a logical volume; 0 for mute.
pub fn volume_gain(percent: u8) -> f32 {
    match volume_db(percent) {
        None => 0.0,
        Some(db) => 10f64.powf(db / 20.0) as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_endpoints() {
        // x = 0 -> 103 - 96 = 7 ; x = 1 -> 103.
        assert_eq!(logical_master_raw(1), 7);
        assert_eq!(logical_master_raw(100), 103);
    }

    #[test]
    fn raw_midpoint() {
        // p = 50: x = 49/99; 96 * (50/99)^1.3 = 39.5 -> rounds to 40.
        assert_eq!(logical_master_raw(50), 103 - 40);
    }

    #[test]
    fn db_range_is_minus_60_to_minus_12() {
        assert_eq!(volume_db(1), Some(-60.0));
        assert_eq!(volume_db(100), Some(-12.0));
        assert_eq!(volume_db(0), None);
    }

    #[test]
    fn gain_is_monotonic_and_mute_is_zero() {
        assert_eq!(volume_gain(0), 0.0);
        let mut prev = 0.0;
        for p in 1..=100u8 {
            let g = volume_gain(p);
            // Neighbouring steps may share a raw index (0.5 dB resolution).
            assert!(g >= prev && g > 0.0, "p={p}");
            prev = g;
        }
        assert!(volume_gain(100) > volume_gain(50) && volume_gain(50) > volume_gain(1));
        assert!((volume_gain(100) - 10f32.powf(-12.0 / 20.0)).abs() < 1e-6);
    }
}
