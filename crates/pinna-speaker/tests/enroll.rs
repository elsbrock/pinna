mod common;

use pinna_speaker::{EnrollError, Model, Voiceprint, WINDOW_SAMPLES, check_speech, enroll, speech};

#[test]
fn speech_gating_keeps_less_than_the_file() {
    let pcm = common::wakeword_wav("alexa-twice.wav");
    let kept = speech(&pcm);
    println!(
        "alexa-twice.wav: {:.1} s of {:.1} s kept",
        kept.len() as f32 / 16000.0,
        pcm.len() as f32 / 16000.0
    );
    assert!(!kept.is_empty() && kept.len() < pcm.len());
}

#[test]
fn the_test_speech_is_long_enough() {
    let s = common::long_speech();
    println!("test speech: {:.1} s", s.len() as f32 / 16000.0);
    assert!(s.len() >= 10 * 16000);
}

#[test]
fn silence_is_too_short() {
    let Err(EnrollError::TooShort { speech_seconds }) = check_speech(&speech(&[0i16; 16000 * 20]))
    else {
        panic!("expected TooShort");
    };
    assert_eq!(speech_seconds, 0.0);
}

#[test]
fn short_speech_reports_its_seconds() {
    let pcm = common::wakeword_wav("alexa-what-time.wav");
    let Err(EnrollError::TooShort { speech_seconds }) = check_speech(&speech(&pcm)) else {
        panic!("expected TooShort");
    };
    assert!(speech_seconds > 0.0 && speech_seconds < 10.0);
    assert!(
        EnrollError::TooShort { speech_seconds }
            .to_string()
            .starts_with("Too short: ")
    );
}

#[test]
fn quiet_speech_is_too_quiet() {
    let quiet: Vec<i16> = common::long_speech().iter().map(|&s| s / 100).collect();
    match check_speech(&quiet) {
        Err(EnrollError::TooQuiet { dbfs }) => {
            assert!(dbfs < -45.0);
            assert!(
                EnrollError::TooQuiet { dbfs }
                    .to_string()
                    .contains("closer to the satellite")
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn clipped_speech_is_clipped() {
    let loud: Vec<i16> = common::long_speech()
        .iter()
        .map(|&s| (i32::from(s) * 50).clamp(-32768, 32767) as i16)
        .collect();
    match check_speech(&loud) {
        Err(EnrollError::Clipped { fraction }) => assert!(fraction > 0.001),
        other => panic!("{other:?}"),
    }
}

#[test]
fn clean_speech_passes_the_checks() {
    assert!(check_speech(&common::long_speech()).is_ok());
}

/// Worst agreement of any window with the mean, as `enroll` computes it.
fn worst_agreement(model: &Model, recording: &[i16]) -> f32 {
    let windows: Vec<_> = speech(recording)
        .chunks_exact(WINDOW_SAMPLES)
        .map(|w| model.voiceprint(w).unwrap())
        .collect();
    let mean = Voiceprint::mean(&windows).unwrap();
    let agreement: Vec<f32> = windows.iter().map(|v| v.cosine(&mean)).collect();
    println!("window agreement: {agreement:.3?}");
    agreement.into_iter().fold(f32::MAX, f32::min)
}

#[test]
fn enrolls_real_speech() {
    let Some(path) = common::model_path() else {
        return;
    };
    let model = Model::load(&path).unwrap();
    // One speaker's file, three times over.
    let recording = common::wakeword_wav("alexa-twice.wav").repeat(3);
    let worst = worst_agreement(&model, &recording);
    let e = enroll(&model, &recording).unwrap();
    println!(
        "enrolled: {} windows, {:.1} s of speech, worst window agreement {worst:.3}",
        e.windows, e.speech_seconds
    );
    assert!(e.windows >= 6);
    assert!(worst >= 0.5);
}

#[test]
fn enrollment_agrees_with_its_windows() {
    let Some(path) = common::model_path() else {
        return;
    };
    let model = Model::load(&path).unwrap();
    // Three files that may not be one voice: enroll accepts exactly when every window agrees.
    let recording = common::recording();
    let worst = worst_agreement(&model, &recording);
    match enroll(&model, &recording) {
        Ok(_) => assert!(worst >= 0.5),
        Err(EnrollError::Inconsistent { worst: w }) => {
            assert!(worst < 0.5 && (w - worst).abs() < 1e-5)
        }
        Err(e) => panic!("{e}"),
    }
}
