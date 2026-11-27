//! Who a voiceprint belongs to, among the enrolled people (ADR 0038).

use crate::Voiceprint;

/// Provisional: the owner's recordings set these (ADR 0038). The spike's same-person scores were
/// about 0.88 and other-person scores about 0.28.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// The least cosine the best match needs.
    pub threshold: f32,
    /// How far the best match must lead the second best.
    pub margin: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            margin: 0.1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Match<Id> {
    Person {
        id: Id,
        score: f32,
        margin: f32,
    },
    Uncertain {
        best: Option<(Id, f32)>,
        margin: f32,
    },
}

/// Provisional, like [`Thresholds`]: two voiceprints at or above this get a dashboard warning,
/// because the satellite will struggle to tell those people apart.
pub const CLOSE_PAIR: f32 = 0.5;

pub fn close(a: &Voiceprint, b: &Voiceprint) -> bool {
    a.cosine(b) >= CLOSE_PAIR
}

/// The best match is a `Person` only if its score is at least `threshold` and it leads the
/// second best by at least `margin`. With one person enrolled the second best counts as a score
/// of 0, so the margin equals the score: a lone voiceprint is judged by the threshold alone
/// unless `margin` is set above it. With nobody enrolled the result is `Uncertain` with no best.
pub fn best_match<Id: Clone>(
    probe: &Voiceprint,
    enrolled: &[(Id, Voiceprint)],
    t: &Thresholds,
) -> Match<Id> {
    let mut best: Option<(&Id, f32)> = None;
    let mut second: Option<f32> = None;
    for (id, vp) in enrolled {
        let s = probe.cosine(vp);
        match best {
            Some((_, b)) if s <= b => second = Some(second.map_or(s, |x| x.max(s))),
            Some((_, b)) => {
                second = Some(b);
                best = Some((id, s));
            }
            None => best = Some((id, s)),
        }
    }
    let Some((id, score)) = best else {
        return Match::Uncertain {
            best: None,
            margin: 0.0,
        };
    };
    let margin = score - second.unwrap_or(0.0);
    if score >= t.threshold && margin >= t.margin {
        Match::Person {
            id: id.clone(),
            score,
            margin,
        }
    } else {
        Match::Uncertain {
            best: Some((id.clone(), score)),
            margin,
        }
    }
}
