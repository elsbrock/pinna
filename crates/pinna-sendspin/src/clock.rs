//! Following the server's clock (ADR 0027 §9): the specification's two-dimensional Kalman
//! time filter, unchanged, and the bursts of `client/time` that feed it.
//!
//! The filter is a port of the C++ reference implementation (github.com/Sendspin/time-filter),
//! checked line by line against the equations and parameters in the specification's
//! messaging.md. All times are µs on the player's monotonic clock or the server's.

/// Offset random-walk diffusion, µs per √µs.
const PROCESS_STD_DEV: f64 = 0.0;
/// Drift random-walk diffusion, 1 per √µs.
const DRIFT_PROCESS_STD_DEV: f64 = 1e-11;
/// Squared, scales the covariances on a large residual.
const FORGET_FACTOR: f64 = 2.0;
/// A residual above this multiple of `max_error` is large.
const ADAPTIVE_CUTOFF: f64 = 3.0;
/// `count` must reach this before adaptive forgetting applies.
const MIN_SAMPLES: u32 = 100;
/// The signal-to-noise ratio drift must exceed before conversions use it.
const DRIFT_SIGNIFICANCE_THRESHOLD: f64 = 2.0;
/// From `max_error` to the measurement's standard deviation.
const MAX_ERROR_SCALE: f64 = 0.5;

/// Requests in one burst: the specification's minimum.
pub const BURST_LEN: u32 = 8;
/// The specification's limit on the time from the start of one burst to the start of the next.
pub const BURST_INTERVAL_MAX_US: i64 = 10_000_000;
/// From the start of one burst to the start of the next. A timer fires at or after its deadline,
/// and the next start is counted from when this one actually began, so the second under
/// [`BURST_INTERVAL_MAX_US`] absorbs that lateness.
pub const BURST_INTERVAL_US: i64 = 9_000_000;
/// How long a request waits for its reply before the next one goes out.
pub const REPLY_TIMEOUT_US: i64 = 500_000;

/// One `server/time` exchange, reduced to what the filter takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// The offset this exchange observed: server minus client.
    pub measurement: i64,
    /// Half the round trip: the worst-case error of `measurement`.
    pub max_error: i64,
    /// When the reply arrived (T4).
    pub time_added: i64,
}

impl Sample {
    /// From the four timestamps: T1 client sent, T2 server received, T3 server sent, T4 client
    /// received. `/ 2` truncates toward zero, as the specification says. `None` when the
    /// server's values overflow.
    pub fn from_timestamps(t1: i64, t2: i64, t3: i64, t4: i64) -> Option<Self> {
        let (t1, t2, t3, t4) = (
            i128::from(t1),
            i128::from(t2),
            i128::from(t3),
            i128::from(t4),
        );
        Some(Self {
            measurement: i64::try_from(((t2 - t1) + (t3 - t4)) / 2).ok()?,
            max_error: i64::try_from(((t4 - t1) - (t3 - t2)) / 2).ok()?,
            time_added: i64::try_from(t4).ok()?,
        })
    }
}

/// The specification's time filter: the server's clock offset and drift.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeFilter {
    offset: f64,
    drift: f64,
    p_oo: f64,
    p_od: f64,
    p_dd: f64,
    last_update: i64,
    count: u32,
    use_drift: bool,
}

impl Default for TimeFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeFilter {
    pub fn new() -> Self {
        Self {
            offset: 0.0,
            drift: 0.0,
            p_oo: f64::INFINITY,
            p_od: 0.0,
            p_dd: 0.0,
            last_update: 0,
            count: 0,
            use_drift: false,
        }
    }

    /// Feeds one measurement. Returns whether the filter took it: a measurement no later than
    /// the last one, or with a `max_error` of 0 or less, is discarded.
    pub fn update(&mut self, sample: Sample) -> bool {
        let Sample {
            measurement,
            max_error,
            time_added,
        } = sample;
        if time_added <= self.last_update || max_error <= 0 {
            return false;
        }
        // Both are positive here, so the difference cannot overflow.
        let dt = (time_added - self.last_update) as f64;
        self.last_update = time_added;
        let measurement = measurement as f64;
        let max_error = max_error as f64;
        let sd = max_error * MAX_ERROR_SCALE;
        let r = sd * sd;

        if self.count == 0 {
            self.offset = measurement;
            self.p_oo = r;
            self.drift = 0.0;
            self.count = 1;
            return true;
        }
        if self.count == 1 {
            self.drift = (measurement - self.offset) / dt;
            self.p_dd = (self.p_oo + r) / (dt * dt);
            self.offset = measurement;
            self.p_oo = r;
            self.count = 2;
            return true;
        }

        let offset_p = self.offset + self.drift * dt;
        let mut p_oo_p = self.p_oo
            + 2.0 * self.p_od * dt
            + self.p_dd * (dt * dt)
            + PROCESS_STD_DEV * PROCESS_STD_DEV * dt;
        let mut p_od_p = self.p_od + self.p_dd * dt;
        let mut p_dd_p = self.p_dd + DRIFT_PROCESS_STD_DEV * DRIFT_PROCESS_STD_DEV * dt;

        let y = measurement - offset_p;
        if self.count < MIN_SAMPLES {
            self.count += 1;
        } else if y.abs() > ADAPTIVE_CUTOFF * max_error {
            let forget = FORGET_FACTOR * FORGET_FACTOR;
            p_oo_p *= forget;
            p_od_p *= forget;
            p_dd_p *= forget;
        }

        // K = P_p / S, written as the reference implementation computes it, so the two agree
        // to the bit.
        let inverse_s = 1.0 / (p_oo_p + r);
        let k_o = p_oo_p * inverse_s;
        let k_d = p_od_p * inverse_s;
        self.offset = offset_p + k_o * y;
        self.drift += k_d * y;
        self.p_oo = p_oo_p - k_o * p_oo_p;
        self.p_od = p_od_p - k_d * p_oo_p;
        self.p_dd = p_dd_p - k_d * p_od_p;

        self.use_drift = self.drift * self.drift
            > DRIFT_SIGNIFICANCE_THRESHOLD * DRIFT_SIGNIFICANCE_THRESHOLD * self.p_dd;
        true
    }

    fn effective_drift(&self) -> f64 {
        if self.use_drift { self.drift } else { 0.0 }
    }

    /// The server's time at the player's time `client_us`.
    pub fn compute_server_time(&self, client_us: i64) -> i64 {
        let dt = client_us.saturating_sub(self.last_update) as f64;
        // `as` saturates, and f64::round rounds halves away from zero, as the specification
        // asks.
        let offset = (self.offset + self.effective_drift() * dt).round() as i64;
        client_us.saturating_add(offset)
    }

    /// The player's time at the server's time `server_us`: the inverse of
    /// [`TimeFilter::compute_server_time`], apart from rounding.
    pub fn compute_client_time(&self, server_us: i64) -> i64 {
        let d = self.effective_drift();
        ((server_us as f64 - self.offset + d * self.last_update as f64) / (1.0 + d)).round() as i64
    }

    /// How many measurements shaped the estimate, up to 100.
    pub fn count(&self) -> u32 {
        self.count
    }

    /// The standard deviation of the offset estimate in µs; infinite before the first
    /// measurement.
    pub fn error_us(&self) -> f64 {
        self.p_oo.sqrt()
    }

    /// The server's clock minus the player's, in µs, at [`TimeFilter::last_update`].
    pub fn offset_us(&self) -> f64 {
        self.offset
    }

    /// How fast the offset changes, in µs per µs.
    pub fn drift(&self) -> f64 {
        self.drift
    }

    /// Whether conversions apply the drift: only once it is significant.
    pub fn uses_drift(&self) -> bool {
        self.use_drift
    }

    /// The player's time of the last measurement taken.
    pub fn last_update(&self) -> i64 {
        self.last_update
    }
}

/// Schedules the bursts of `client/time` and picks each burst's best measurement.
///
/// A burst sends [`BURST_LEN`] requests one after another, the next as soon as the previous
/// one's reply arrives or after [`REPLY_TIMEOUT_US`] without one. Only the measurement with
/// the lowest positive `max_error` goes into the filter, once the burst is over. Bursts start
/// [`BURST_INTERVAL_US`] apart.
#[derive(Debug, Clone, Default)]
pub struct Bursts {
    next_start: Option<i64>,
    current: Option<Burst>,
}

#[derive(Debug, Clone)]
struct Burst {
    sent: u32,
    /// The latest request's T1 and when the next may go without its reply.
    waiting: Option<(i64, i64)>,
    /// T1 of every request of this burst still unanswered: a reply after its timeout still
    /// measures the clock.
    unanswered: Vec<i64>,
    best: Option<Sample>,
}

/// What [`Bursts::poll`] asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BurstStep {
    /// Send `client/time` with this `client_transmitted`.
    pub send: Option<i64>,
    /// A burst ended: feed this measurement to the filter.
    pub feed: Option<Sample>,
}

impl Bursts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedules the first burst for `now`.
    pub fn start(&mut self, now: i64) {
        if self.next_start.is_none() && self.current.is_none() {
            self.next_start = Some(now);
        }
    }

    /// Takes a `server/time` that arrived at `now` (T4).
    pub fn on_reply(&mut self, reply: &crate::messages::ServerTime, now: i64) {
        let Some(burst) = self.current.as_mut() else {
            return;
        };
        let Some(i) = burst
            .unanswered
            .iter()
            .position(|t1| *t1 == reply.client_transmitted)
        else {
            return;
        };
        burst.unanswered.swap_remove(i);
        if burst
            .waiting
            .is_some_and(|(t1, _)| t1 == reply.client_transmitted)
        {
            burst.waiting = None;
        }
        let sample = Sample::from_timestamps(
            reply.client_transmitted,
            reply.server_received,
            reply.server_transmitted,
            now,
        );
        if let Some(s) = sample.filter(|s| s.max_error > 0)
            && burst.best.is_none_or(|b| s.max_error < b.max_error)
        {
            burst.best = Some(s);
        }
    }

    /// What to do at `now`.
    pub fn poll(&mut self, now: i64) -> BurstStep {
        let mut step = BurstStep::default();
        if self.current.is_none() {
            match self.next_start {
                Some(at) if at <= now => {
                    self.next_start = Some(now + BURST_INTERVAL_US);
                    self.current = Some(Burst {
                        sent: 0,
                        waiting: None,
                        unanswered: Vec::new(),
                        best: None,
                    });
                }
                _ => return step,
            }
        }
        let Some(burst) = self.current.as_mut() else {
            return step;
        };
        if burst.waiting.is_some_and(|(_, until)| now >= until) {
            burst.waiting = None;
        }
        if burst.waiting.is_some() {
            return step;
        }
        if burst.sent < BURST_LEN {
            burst.sent += 1;
            burst.waiting = Some((now, now + REPLY_TIMEOUT_US));
            burst.unanswered.push(now);
            step.send = Some(now);
        } else {
            step.feed = burst.best;
            self.current = None;
        }
        step
    }

    /// When [`Bursts::poll`] has something to do next, if it was polled after every change.
    pub fn next_wakeup(&self) -> Option<i64> {
        match &self.current {
            Some(burst) => burst.waiting.map(|(_, until)| until),
            None => self.next_start,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::ServerTime;

    #[test]
    fn sample_from_timestamps() {
        // Server 1000 µs ahead, 100 µs each way, 20 µs in the server.
        let s = Sample::from_timestamps(10_000, 11_100, 11_120, 10_220).unwrap();
        assert_eq!(
            s,
            Sample {
                measurement: 1000,
                max_error: 100,
                time_added: 10_220
            }
        );
        // Odd sums truncate toward zero, also below zero.
        let s = Sample::from_timestamps(0, -3, -3, 0).unwrap();
        assert_eq!((s.measurement, s.max_error), (-3, 0));
        let s = Sample::from_timestamps(0, 0, 0, 3).unwrap();
        assert_eq!((s.measurement, s.max_error), (-1, 1));
        assert!(Sample::from_timestamps(i64::MIN, i64::MAX, i64::MAX, i64::MIN).is_none());
    }

    #[test]
    fn first_two_measurements_by_hand() {
        // Derived by hand from steps 3 and 4 of the specification's update.
        let mut f = TimeFilter::new();
        assert_eq!(f.error_us(), f64::INFINITY);
        assert!(f.update(Sample {
            measurement: 1000,
            max_error: 200,
            time_added: 1_000_000
        }));
        assert_eq!((f.offset_us(), f.drift(), f.count()), (1000.0, 0.0, 1));
        assert_eq!(f.error_us(), 100.0); // sqrt((0.5 * 200)^2)
        assert!(f.update(Sample {
            measurement: 1100,
            max_error: 400,
            time_added: 2_000_000
        }));
        assert_eq!(f.drift(), 100.0 / 1_000_000.0);
        assert_eq!(f.offset_us(), 1100.0);
        assert_eq!(f.error_us(), 200.0);
        assert_eq!(f.p_dd, (10_000.0 + 40_000.0) / 1e12);
        assert_eq!(f.p_od, 0.0);
        assert!(!f.uses_drift());
        // Without drift in use, conversions are the offset alone.
        assert_eq!(f.compute_server_time(5_000_000), 5_001_100);
        assert_eq!(f.compute_client_time(5_001_100), 5_000_000);
    }

    #[test]
    fn discarded_measurements() {
        let mut f = TimeFilter::new();
        let s = |time_added, max_error| Sample {
            measurement: 7,
            max_error,
            time_added,
        };
        // A filter starts at last_update 0.
        assert!(!f.update(s(0, 10)));
        assert!(!f.update(s(5, 0)));
        assert!(!f.update(s(5, -1)));
        assert!(f.update(s(5, 10)));
        assert!(!f.update(s(5, 10)));
        assert!(!f.update(s(4, 10)));
        assert_eq!(f.count(), 1);
    }

    /// Values from the C++ reference implementation (github.com/Sendspin/time-filter, built
    /// with g++ -O0), for 130 measurements one second apart: offset 1 s plus 50 ppm drift,
    /// deterministic noise of ±400 µs, `max_error` 500 to 899 µs, a 5 ms step at k = 115 that
    /// sets off adaptive forgetting, and two invalid measurements after k = 5. Columns: k,
    /// offset, drift, P_oo, P_od, P_dd, count, use_drift, compute_client_time(50 s + k s),
    /// compute_server_time(2 s + k s), round(sqrt(P_oo)).
    #[allow(clippy::type_complexity, clippy::excessive_precision)]
    const REFERENCE: &[(u32, f64, f64, f64, f64, f64, u32, bool, i64, i64, i64)] = &[
        (
            0, 999650.0, 0.0, 62500.0, 0.0, 0.0, 1, false, 49000350, 2999650, 250,
        ),
        (
            1,
            1000410.0,
            0.00076000000000000004,
            72092.25,
            0.0,
            1.3459225e-07,
            2,
            false,
            49999590,
            4000410,
            269,
        ),
        (
            2,
            1000597.2538319031,
            0.00038702962513859898,
            58897.040099843114,
            0.038353554066115791,
            7.1921935573234195e-08,
            3,
            false,
            50999403,
            5000597,
            243,
        ),
        (
            3,
            1000531.5891960188,
            0.0001464763396149033,
            64377.774727535609,
            0.034209148555181035,
            3.1501701000468932e-08,
            4,
            false,
            51999468,
            6000532,
            254,
        ),
        (
            5,
            1000361.6239861113,
            1.0550258632291289e-05,
            61832.383299379653,
            0.019428330281105487,
            8.6620043842160901e-09,
            6,
            false,
            53999638,
            8000362,
            249,
        ),
        (
            50,
            1002487.2870754163,
            4.748735270259568e-05,
            8657.9170493034289,
            0.00025796584314275828,
            1.0316132035854484e-11,
            51,
            true,
            98995234,
            53002535,
            93,
        ),
        (
            99,
            1004977.1674508507,
            4.9619772161172764e-05,
            4425.8152051248517,
            6.676480055253381e-05,
            1.3533953843221886e-12,
            100,
            true,
            147992641,
            102005027,
            67,
        ),
        (
            100,
            1005033.3085863327,
            4.9717174682634783e-05,
            4340.734004206779,
            6.4832829610096771e-05,
            1.3044254355386178e-12,
            100,
            true,
            148992581,
            103005083,
            66,
        ),
        (
            114,
            1005709.3276171228,
            4.945733675684013e-05,
            3884.1540320112831,
            5.0926971369710633e-05,
            8.9609112946157737e-13,
            100,
            true,
            162991917,
            117005759,
            62,
        ),
        (
            115,
            1006301.2840620455,
            5.6508914804283007e-05,
            14342.565717984939,
            0.00018642928631329618,
            3.3135809997538745e-12,
            100,
            true,
            163990987,
            118006358,
            120,
        ),
        (
            116,
            1007654.4083708252,
            7.3223902515944032e-05,
            42804.458549011455,
            0.00055180279516608575,
            1.0584061705724794e-11,
            100,
            true,
            164988832,
            119007728,
            207,
        ),
        (
            129,
            1011713.4824660076,
            0.00010790642339223781,
            10397.445237197529,
            0.00018827905737218845,
            1.289682970680675e-11,
            100,
            true,
            177983109,
            132011821,
            102,
        ),
    ];

    fn close(a: f64, b: f64) -> bool {
        a == b || (a - b).abs() <= 1e-12 * a.abs().max(b.abs())
    }

    #[test]
    fn matches_the_reference_implementation() {
        let mut f = TimeFilter::new();
        let mut reference = REFERENCE.iter();
        let mut next = reference.next();
        for k in 0..130i64 {
            let t = 1_000_000 + 1_000_000 * k;
            let step = if k >= 115 { 5000.0 } else { 0.0 };
            let truth = 1_000_000.0 + 50e-6 * t as f64 + step;
            let noise = (k * 7919) % 801 - 400;
            let measurement = truth as i64 + noise;
            let max_error = 500 + (k * 37) % 400;
            f.update(Sample {
                measurement,
                max_error,
                time_added: t,
            });
            if k == 5 {
                // Neither changes anything: not later, and no error bound.
                assert!(!f.update(Sample {
                    measurement: measurement + 99_999,
                    max_error,
                    time_added: t
                }));
                assert!(!f.update(Sample {
                    measurement,
                    max_error: 0,
                    time_added: t + 10
                }));
            }
            let Some(&(rk, offset, drift, p_oo, p_od, p_dd, count, use_drift, client, server, err)) =
                next
            else {
                break;
            };
            if i64::from(rk) != k {
                continue;
            }
            assert!(
                close(f.offset, offset),
                "k={k} offset {} {offset}",
                f.offset
            );
            assert!(close(f.drift, drift), "k={k} drift {} {drift}", f.drift);
            assert!(close(f.p_oo, p_oo), "k={k} p_oo {} {p_oo}", f.p_oo);
            assert!(close(f.p_od, p_od), "k={k} p_od {} {p_od}", f.p_od);
            assert!(close(f.p_dd, p_dd), "k={k} p_dd {} {p_dd}", f.p_dd);
            assert_eq!(f.count(), count, "k={k}");
            assert_eq!(f.uses_drift(), use_drift, "k={k}");
            assert_eq!(
                f.compute_client_time(50_000_000 + 1_000_000 * k),
                client,
                "k={k}"
            );
            assert_eq!(
                f.compute_server_time(2_000_000 + 1_000_000 * k),
                server,
                "k={k}"
            );
            assert_eq!(f.error_us().round() as i64, err, "k={k}");
            next = reference.next();
        }
        assert!(next.is_none(), "not every reference row was checked");
    }

    #[test]
    fn conversions_invert_each_other_with_drift() {
        let mut f = TimeFilter::new();
        for k in 0..150i64 {
            let t = 1_000_000 + 1_000_000 * k;
            f.update(Sample {
                measurement: 3_000_000 + t / 10_000,
                max_error: 300,
                time_added: t,
            });
        }
        assert!(f.uses_drift());
        assert!((f.drift() - 1e-4).abs() < 1e-7, "{}", f.drift());
        for client in [0, 151_000_000, 400_000_000] {
            let server = f.compute_server_time(client);
            assert!((f.compute_client_time(server) - client).abs() <= 1);
        }
        // Extreme inputs saturate instead of panicking.
        f.compute_client_time(i64::MIN);
        f.compute_client_time(i64::MAX);
        f.compute_server_time(i64::MIN);
        f.compute_server_time(i64::MAX);
    }

    fn reply(t1: i64, offset: i64, one_way: i64) -> ServerTime {
        ServerTime {
            client_transmitted: t1,
            server_received: t1 + one_way + offset,
            server_transmitted: t1 + one_way + offset + 10,
        }
    }

    #[test]
    fn a_burst_sends_eight_requests_one_after_another() {
        let mut b = Bursts::new();
        assert_eq!(b.poll(0), BurstStep::default());
        assert_eq!(b.next_wakeup(), None);
        b.start(1000);
        assert_eq!(b.next_wakeup(), Some(1000));
        assert_eq!(b.poll(999), BurstStep::default());

        let mut now = 1000;
        let mut sent = Vec::new();
        for i in 0..BURST_LEN {
            let step = b.poll(now);
            assert_eq!(step.feed, None);
            assert_eq!(step.send, Some(now), "request {i}");
            sent.push(now);
            // Nothing more until the reply or the timeout.
            assert_eq!(b.poll(now + 1), BurstStep::default());
            assert_eq!(b.next_wakeup(), Some(now + REPLY_TIMEOUT_US));
            // Round trips of 2000, 1000, 3000, ... µs; the second is the best.
            let rtt = [2000, 1000, 3000, 4000, 5000, 6000, 7000, 8000][i as usize];
            now += rtt;
            b.on_reply(&reply(sent[i as usize], 50_000, rtt / 2 - 5), now);
        }
        let step = b.poll(now);
        assert_eq!(step.send, None);
        let best = step.feed.unwrap();
        assert_eq!(best.max_error, 495);
        assert_eq!(best.measurement, 50_000);
        assert_eq!(best.time_added, 1000 + 2000 + 1000);
        // The next burst starts BURST_INTERVAL_US after this one started.
        assert_eq!(b.next_wakeup(), Some(1000 + BURST_INTERVAL_US));
        assert_eq!(b.poll(1000 + BURST_INTERVAL_US - 1), BurstStep::default());
        assert_eq!(
            b.poll(1000 + BURST_INTERVAL_US).send,
            Some(1000 + BURST_INTERVAL_US)
        );
    }

    #[test]
    fn bursts_start_within_the_limit_when_every_wakeup_is_late() {
        let mut b = Bursts::new();
        b.start(0);
        // pinnad's timer fires 7 ms after each deadline, and every reply takes 1 ms.
        let late = 7_000;
        let mut now = 0;
        let mut starts = Vec::new();
        while starts.len() < 5 {
            now = b.next_wakeup().unwrap() + late;
            let mut step = b.poll(now);
            if b.current.as_ref().is_some_and(|burst| burst.sent == 1) {
                starts.push(now);
            }
            while let Some(t1) = step.send {
                now += 1_000;
                b.on_reply(&reply(t1, 0, 450), now);
                step = b.poll(now);
            }
        }
        for pair in starts.windows(2) {
            assert!(pair[1] - pair[0] <= BURST_INTERVAL_MAX_US, "{starts:?}");
        }
    }

    #[test]
    fn a_lost_reply_times_out_and_a_late_one_still_counts() {
        let mut b = Bursts::new();
        b.start(0);
        assert_eq!(b.poll(0).send, Some(0));
        // No reply: the next request waits 500 ms.
        assert_eq!(b.poll(REPLY_TIMEOUT_US - 1).send, None);
        assert_eq!(b.poll(REPLY_TIMEOUT_US).send, Some(REPLY_TIMEOUT_US));
        // The first request's reply arrives late. It still measures the clock, if poorly, and
        // does not release the request in flight.
        b.on_reply(&reply(0, 7, 1), 600_000);
        assert_eq!(b.poll(600_000).send, None);
        // A reply nobody asked for is ignored.
        b.on_reply(&reply(123, 0, 0), 600_001);
        let mut last = REPLY_TIMEOUT_US;
        for i in 2..BURST_LEN {
            let now = 600_000 + i64::from(i);
            // The server claims the whole round trip: max_error 0, never fed.
            b.on_reply(
                &ServerTime {
                    client_transmitted: last,
                    server_received: last,
                    server_transmitted: now,
                },
                now,
            );
            last = b.poll(now).send.unwrap();
        }
        // The eighth request gets no reply.
        let step = b.poll(last + REPLY_TIMEOUT_US);
        assert_eq!(step.send, None);
        assert_eq!(
            step.feed.map(|s| (s.measurement, s.max_error)),
            Some((-299_987, 299_995))
        );
        assert!(b.current.is_none());
    }

    #[test]
    fn a_burst_without_a_valid_reply_feeds_nothing() {
        let mut b = Bursts::new();
        b.start(0);
        for i in 0..BURST_LEN {
            assert!(b.poll(i64::from(i) * REPLY_TIMEOUT_US).send.is_some());
        }
        assert_eq!(
            b.poll(i64::from(BURST_LEN) * REPLY_TIMEOUT_US),
            BurstStep::default()
        );
        assert_eq!(b.next_wakeup(), Some(BURST_INTERVAL_US));
    }
}
