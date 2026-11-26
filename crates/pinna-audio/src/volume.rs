//! Playback volume curves: LibreEcho's (audio_engine.c), and a stock
//! firmware's steps where the board has them.

/// How a volume of 0 to 100 % becomes a gain: the board's
/// `pinna_board::VolumeCurve`, which pinnad maps here (pinna-audio doesn't
/// depend on the board crate, ADR 0032).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VolumeCurve {
    /// LibreEcho's curve ([`volume_db`]).
    LibreEcho,
    /// A stock firmware's volume steps in dB, from the first step above
    /// mute to the top. 1 to 100 % spread evenly over them, in dB between
    /// two steps.
    Steps(&'static [f32]),
}

/// Gain in dB for a volume on `curve`. `None` for 0 (mute).
pub fn curve_db(curve: VolumeCurve, percent: u8) -> Option<f64> {
    match curve {
        VolumeCurve::LibreEcho => volume_db(percent),
        VolumeCurve::Steps(steps) => {
            if percent == 0 || steps.is_empty() {
                return None;
            }
            let last = steps.len() - 1;
            let at = f64::from(percent.min(100) - 1) / 99.0 * last as f64;
            let i = (at.floor() as usize).min(last);
            let j = (i + 1).min(last);
            let f = at - i as f64;
            Some(f64::from(steps[i]) * (1.0 - f) + f64::from(steps[j]) * f)
        }
    }
}

/// Linear amplitude gain for a volume on `curve`; 0 for mute.
pub fn curve_gain(curve: VolumeCurve, percent: u8) -> f32 {
    match curve_db(curve, percent) {
        None => 0.0,
        Some(db) => 10f64.powf(db / 20.0) as f32,
    }
}

/// [`stream_gain`] on `curve`.
pub fn curve_stream_gain(curve: VolumeCurve, percent: u8) -> f32 {
    curve_gain(curve, percent) / curve_gain(curve, 100)
}

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

/// Gain of a live stream's own volume 0..=100 (the sender's volume, ADR
/// 0011): the master curve relative to its top, so 100 plays at full level
/// and 0 is silent. The master volume applies on top.
pub fn stream_gain(percent: u8) -> f32 {
    volume_gain(percent) / volume_gain(100)
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
    use pinna_board::SpeakerTuning;

    /// What pinnad hands the speaker for a board's curve.
    fn of(curve: pinna_board::VolumeCurve) -> VolumeCurve {
        match curve {
            pinna_board::VolumeCurve::LibreEcho => VolumeCurve::LibreEcho,
            pinna_board::VolumeCurve::Steps(s) => VolumeCurve::Steps(s),
        }
    }

    #[test]
    fn libreecho_curve_is_the_master_curve() {
        for p in 0..=100u8 {
            assert_eq!(curve_db(VolumeCurve::LibreEcho, p), volume_db(p), "p={p}");
            assert_eq!(
                curve_gain(VolumeCurve::LibreEcho, p),
                volume_gain(p),
                "p={p}"
            );
        }
        for t in [SpeakerTuning::FLAT, SpeakerTuning::RADAR] {
            assert_eq!(of(t.volume_curve), VolumeCurve::LibreEcho);
        }
    }

    #[test]
    fn echo_dot_5_follows_fire_os_steps() {
        let curve = of(SpeakerTuning::ECHO_DOT_5.volume_curve);
        let db = |p| curve_db(curve, p).unwrap();
        assert_eq!(curve_db(curve, 0), None);
        // Fire OS's step 1 and its top step.
        assert!((db(1) - -54.21).abs() < 1e-4);
        assert!((db(100) - 0.0).abs() < 1e-9);
        // Half volume lies between steps 15 (-11.48) and 16 (-9.35), where
        // LibreEcho's curve gave -32 dB.
        assert!((db(50) - -10.735).abs() < 0.01, "{}", db(50));
        let mut prev = f64::NEG_INFINITY;
        for p in 1..=100u8 {
            assert!(db(p) >= prev, "p={p}: {prev} -> {}", db(p));
            prev = db(p);
        }
        assert_eq!(curve_stream_gain(curve, 100), 1.0);
        assert_eq!(curve_stream_gain(curve, 0), 0.0);
    }

    #[test]
    fn steps_spread_from_one_to_a_hundred_percent() {
        const TWO: [f32; 2] = [-40.0, 0.0];
        let curve = VolumeCurve::Steps(&TWO);
        assert_eq!(curve_db(curve, 1), Some(-40.0));
        assert_eq!(curve_db(curve, 100), Some(0.0));
        assert!((curve_db(curve, 50).unwrap() - -40.0 * 50.0 / 99.0).abs() < 1e-9);
        assert_eq!(curve_db(VolumeCurve::Steps(&[]), 50), None);
    }

    #[test]
    fn stream_gain_is_the_master_curve_from_full_level() {
        assert_eq!(stream_gain(100), 1.0);
        assert_eq!(stream_gain(0), 0.0);
        let mut prev = 0.0;
        for p in 1..=100 {
            let g = stream_gain(p);
            assert!(g > 0.0 && g >= prev && g <= 1.0, "p={p}: {g}");
            prev = g;
        }
        // Same steps as the master volume, shifted to 0 dB at the top.
        let db = 20.0 * f64::from(stream_gain(50)).log10();
        assert!((db - (volume_db(50).unwrap() - volume_db(100).unwrap())).abs() < 1e-3);
    }

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
