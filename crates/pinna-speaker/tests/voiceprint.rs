use pinna_speaker::{CLOSE_PAIR, Match, Thresholds, Voiceprint, best_match, close};

fn vp(v: &[f32]) -> Voiceprint {
    Voiceprint::from_le_bytes(&v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap()
}

/// A unit vector at `angle` radians from the first axis.
fn at(angle: f32) -> Voiceprint {
    vp(&[angle.cos(), angle.sin(), 0.0])
}

#[test]
fn nobody_enrolled_is_uncertain() {
    let m = best_match::<u32>(&at(0.0), &[], &Thresholds::default());
    assert_eq!(
        m,
        Match::Uncertain {
            best: None,
            margin: 0.0
        }
    );
}

#[test]
fn one_person_over_and_under_the_threshold() {
    let t = Thresholds::default();
    let enrolled = [(1, at(0.0))];
    // cos(0.3) = 0.955
    match best_match(&at(0.3), &enrolled, &t) {
        Match::Person { id, score, margin } => {
            assert_eq!(id, 1);
            assert!((score - 0.3f32.cos()).abs() < 1e-5);
            // The missing second counts as 0, so the margin is the score.
            assert!((margin - score).abs() < 1e-6);
        }
        other => panic!("{other:?}"),
    }
    // cos(1.3) = 0.27
    match best_match(&at(1.3), &enrolled, &t) {
        Match::Uncertain {
            best: Some((1, s)), ..
        } => assert!(s < t.threshold),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_margin_decides_between_two_people() {
    let t = Thresholds::default();
    // The probe sits between them: both score high, the lead is small.
    let enrolled = [(1, at(0.0)), (2, at(0.4))];
    let far = [(1, at(0.0)), (2, at(0.8))];
    match best_match(&at(0.22), &enrolled, &t) {
        Match::Uncertain {
            best: Some(_),
            margin,
        } => assert!(margin < t.margin),
        other => panic!("{other:?}"),
    }
    // Close to the first only.
    match best_match(&at(0.02), &far, &t) {
        Match::Person { id, margin, .. } => {
            assert_eq!(id, 1);
            assert!(margin >= t.margin);
        }
        other => panic!("{other:?}"),
    }
    // Order doesn't matter.
    let swapped = [(2, at(0.8)), (1, at(0.0))];
    assert!(matches!(
        best_match(&at(0.02), &swapped, &t),
        Match::Person { id: 1, .. }
    ));
}

#[test]
fn a_tie_is_uncertain() {
    let enrolled = [(1, at(0.0)), (2, at(0.0))];
    match best_match(&at(0.0), &enrolled, &Thresholds::default()) {
        Match::Uncertain {
            best: Some(_),
            margin,
        } => assert!(margin.abs() < 1e-6),
        other => panic!("{other:?}"),
    }
}

#[test]
fn close_pairs() {
    assert!(close(&at(0.0), &at(0.5)));
    assert!(!close(&at(0.0), &at(1.5)));
    assert_eq!(CLOSE_PAIR, 0.5);
}

#[test]
fn bytes_round_trip() {
    let a = vp(&[3.0, 4.0, 0.0]);
    assert!((a.cosine(&a) - 1.0).abs() < 1e-6);
    let b = Voiceprint::from_le_bytes(&a.to_le_bytes()).unwrap();
    assert_eq!(a, b);
    assert_eq!(b.dim(), 3);
}

#[test]
fn bad_bytes_are_rejected() {
    assert!(Voiceprint::from_le_bytes(&[]).is_err());
    assert!(Voiceprint::from_le_bytes(&[0; 7]).is_err());
    assert!(Voiceprint::from_le_bytes(&f32::NAN.to_le_bytes()).is_err());
    assert!(Voiceprint::from_le_bytes(&f32::INFINITY.to_le_bytes()).is_err());
}

#[test]
fn the_mean_is_normalised() {
    assert!(Voiceprint::mean(&[]).is_none());
    let m = Voiceprint::mean(&[vp(&[1.0, 0.0]), vp(&[0.0, 1.0])]).unwrap();
    assert!((m.cosine(&m) - 1.0).abs() < 1e-6);
    assert!((m.as_slice()[0] - m.as_slice()[1]).abs() < 1e-6);
}
