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

/// Extra Echo Dot speaker gain at the top of the volume range, in dB: none up
/// to 70 %, then the stock firmware's measured steps (+3.52 dB at 80 %, +7.54
/// at 90 %, +14.54 at 100 %), interpolated between them so that one volume
/// step never jumps by several dB. The speaker chain's limiter keeps the
/// result under full scale.
pub fn top_boost_db(percent: u8) -> f32 {
    const STEPS: [(u8, f32); 4] = [(70, 0.0), (80, 3.52), (90, 7.54), (100, 14.54)];
    let p = percent.min(100);
    if p <= STEPS[0].0 {
        return 0.0;
    }
    for w in STEPS.windows(2) {
        let ((p0, d0), (p1, d1)) = (w[0], w[1]);
        if p <= p1 {
            return d0 + (d1 - d0) * f32::from(p - p0) / f32::from(p1 - p0);
        }
    }
    STEPS[STEPS.len() - 1].1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_boost_follows_the_stock_steps() {
        for p in [0, 1, 50, 70] {
            assert_eq!(top_boost_db(p), 0.0, "p={p}");
        }
        assert!((top_boost_db(80) - 3.52).abs() < 1e-4);
        assert!((top_boost_db(90) - 7.54).abs() < 1e-4);
        assert!((top_boost_db(100) - 14.54).abs() < 1e-4);
        assert!((top_boost_db(85) - 5.53).abs() < 1e-4);
        let mut prev = 0.0;
        for p in 70..=100u8 {
            let d = top_boost_db(p);
            assert!(d >= prev && d - prev < 0.71, "p={p}: {prev} -> {d}");
            prev = d;
        }
    }

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
