use std::net::Ipv4Addr;

use connectrpc::ErrorCode;

use super::*;
use crate::standalone::service::Paths;
use crate::voice::Snapshot;

const WHO: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9));
const PASSWORD: &str = "correct horse";

pub(crate) struct Rig {
    pub auth: Arc<Auth>,
    pub service: Arc<Service>,
    pub tx: mpsc::Sender<Input>,
    pub inputs: mpsc::Receiver<Input>,
    pub dir: tempfile::TempDir,
}

pub(crate) fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let status = watch::channel(Snapshot::default()).1;
    let service = Service::load(Paths::beside(&dir.path().join("settings.toml")), status);
    let (tx, inputs) = mpsc::channel(8);
    Rig {
        auth: Auth::new(service.clone(), "02:00:00:aa:bb:cc", tx.clone()),
        service,
        tx,
        inputs,
        dir,
    }
}

impl Rig {
    /// An attempt was taken just now: the next one waits, however slow this
    /// machine is.
    pub fn take_the_turn(&self) {
        let later = Instant::now() + Duration::from_secs(3600);
        self.auth.limiter.lock().unwrap().admit(later).unwrap();
    }

    /// Someone presses Action at the satellite: the grant that gives.
    pub async fn press(&mut self) -> String {
        self.press_for(Purpose::Password, None).await
    }

    /// The same for a confirmation of `purpose`, asked for from `session`.
    pub async fn press_for(&mut self, purpose: Purpose, session: Option<&str>) -> String {
        // The minute between two confirmations isn't what these tests are about.
        self.auth.confirmations.lock().unwrap().current = None;
        let (id, _) = self.auth.start_confirmation(WHO, purpose, session).unwrap();
        let mut progress = self.auth.watch_confirmation(&id).unwrap();
        let Some(Input::Presence(presence)) = self.inputs.recv().await else {
            panic!("the Brain was not asked to wait for a press");
        };
        presence.pressed.send(()).unwrap();
        loop {
            progress.changed().await.unwrap();
            let state = progress.borrow().clone();
            if let ConfirmProgress::Confirmed(grant) = state {
                return grant;
            }
        }
    }

    /// A password, set the way the first one is.
    pub async fn with_password(&mut self) {
        let grant = self.press().await;
        self.auth
            .set_password(WHO, PASSWORD.into(), String::new(), grant)
            .await
            .unwrap();
    }
}

fn code(e: ConnectError) -> ErrorCode {
    e.code
}

fn cookie(value: &str) -> axum::http::HeaderMap {
    let mut h = axum::http::HeaderMap::new();
    h.insert(
        "cookie",
        format!("pinna_session_aabbcc={value}").parse().unwrap(),
    );
    h
}

#[tokio::test]
async fn the_first_password_needs_a_press() {
    let mut rig = rig();
    let e = rig
        .auth
        .set_password(WHO, PASSWORD.into(), String::new(), String::new())
        .await
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::FailedPrecondition);
    let e = rig
        .auth
        .set_password(WHO, PASSWORD.into(), String::new(), "made up".into())
        .await
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::PermissionDenied);
    assert!(!rig.auth.security(false).password_set);

    let grant = rig.press().await;
    rig.auth
        .set_password(WHO, PASSWORD.into(), String::new(), grant.clone())
        .await
        .unwrap();
    assert!(rig.auth.security(false).password_set);
    // A grant works once.
    let e = rig
        .auth
        .set_password(WHO, "another one".into(), String::new(), grant)
        .await
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn a_password_is_8_to_128_characters() {
    let rig = rig();
    for bad in ["", "1234567", &"x".repeat(129)] {
        let e = rig
            .auth
            .set_password(WHO, bad.into(), String::new(), String::new())
            .await
            .unwrap_err();
        assert_eq!(code(e), ErrorCode::InvalidArgument, "{} chars", bad.len());
    }
}

#[tokio::test]
async fn the_hash_is_in_secrets_toml_and_the_file_stays_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let mut rig = rig();
    rig.with_password().await;
    let path = rig.dir.path().join("secrets.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[dashboard]"), "{text}");
    assert!(text.contains("iterations = 100000"), "{text}");
    assert!(!text.contains(PASSWORD), "{text}");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // A restart reads it back.
    let again = Auth::new(
        Service::load(
            Paths::beside(&rig.dir.path().join("settings.toml")),
            watch::channel(Snapshot::default()).1,
        ),
        "02:00:00:aa:bb:cc",
        mpsc::channel(1).0,
    );
    assert!(again.security(false).password_set);
    assert!(again.login(WHO, PASSWORD.into()).await.is_ok());
}

#[tokio::test]
async fn a_login_gives_a_session_that_a_new_key_ends() {
    let mut rig = rig();
    rig.with_password().await;
    assert_eq!(
        rig.auth.login(WHO, "wrong".into()).await,
        Err(LoginError::Wrong)
    );
    // One attempt a second for the whole satellite.
    rig.take_the_turn();
    assert!(matches!(
        rig.auth.login(WHO, PASSWORD.into()).await,
        Err(LoginError::Wait(_))
    ));
    rig.auth.limiter.lock().unwrap().reset();
    let value = rig.auth.login(WHO, PASSWORD.into()).await.unwrap();
    assert!(rig.auth.signed_in(&cookie(&value)));
    assert!(!rig.auth.signed_in(&cookie("garbage")));
    assert!(!rig.auth.signed_in(&axum::http::HeaderMap::new()));

    let end = rig.auth.subscribe_end();
    rig.auth.sign_out_everywhere().await.unwrap();
    assert!(end.has_changed().unwrap());
    assert!(!rig.auth.signed_in(&cookie(&value)));
}

#[tokio::test]
async fn failures_are_counted_with_where_they_came_from() {
    let mut rig = rig();
    rig.with_password().await;
    let _ = rig.auth.login(WHO, "wrong".into()).await;
    let s = rig.auth.security(true);
    assert_eq!(s.failures_since_login, 1);
    assert_eq!(s.last_failure_address, "192.168.1.9");
    assert!(s.last_failure_unix_ms > 0);
    assert!(s.login_retry_after_s <= 1, "{s:?}");
    rig.auth.limiter.lock().unwrap().reset();
    rig.auth.login(WHO, PASSWORD.into()).await.unwrap();
    assert_eq!(rig.auth.security(true).failures_since_login, 0);
}

#[tokio::test]
async fn a_refused_attempt_runs_no_pbkdf2_and_counts_for_nothing() {
    let mut rig = rig();
    rig.with_password().await;
    let _ = rig.auth.login(WHO, "wrong".into()).await;
    rig.take_the_turn();
    // Hundreds of attempts in the same second: none is checked.
    let start = std::time::Instant::now();
    for _ in 0..200 {
        assert!(matches!(
            rig.auth.login(WHO, "wrong".into()).await,
            Err(LoginError::Wait(_))
        ));
    }
    // 200 checks at this cost would take seconds.
    assert!(start.elapsed() < Duration::from_millis(500));
    assert_eq!(rig.auth.security(true).failures_since_login, 1);
}

#[tokio::test]
async fn a_change_needs_the_current_password_or_a_press() {
    let mut rig = rig();
    rig.with_password().await;
    let e = rig
        .auth
        .set_password(WHO, "new password".into(), String::new(), String::new())
        .await
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::InvalidArgument);
    let e = rig
        .auth
        .set_password(WHO, "new password".into(), "wrong".into(), String::new())
        .await
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::PermissionDenied);
    assert_eq!(
        rig.auth.security(true).failures_since_login,
        1,
        "a wrong current password is a failed login"
    );

    // The forgotten password: a press sets a new one without it.
    let grant = rig.press().await;
    rig.auth
        .set_password(WHO, "new password".into(), String::new(), grant)
        .await
        .unwrap();
    rig.auth.limiter.lock().unwrap().reset();
    assert!(rig.auth.login(WHO, "new password".into()).await.is_ok());
}

#[tokio::test]
async fn a_change_ends_every_session() {
    let mut rig = rig();
    rig.with_password().await;
    let value = rig.auth.login(WHO, PASSWORD.into()).await.unwrap();
    let end = rig.auth.subscribe_end();
    rig.auth.limiter.lock().unwrap().reset();
    rig.auth
        .set_password(WHO, "new password".into(), PASSWORD.into(), String::new())
        .await
        .unwrap();
    assert!(end.has_changed().unwrap());
    assert!(!rig.auth.signed_in(&cookie(&value)));
}

#[tokio::test]
async fn of_two_changes_at_once_the_second_has_a_wrong_current_password() {
    let mut rig = rig();
    rig.with_password().await;
    let (a, b) = tokio::join!(
        rig.auth
            .set_password(WHO, "first change".into(), PASSWORD.into(), String::new()),
        rig.auth
            .set_password(WHO, "second change".into(), PASSWORD.into(), String::new()),
    );
    a.unwrap();
    assert_eq!(code(b.unwrap_err()), ErrorCode::PermissionDenied);
    assert_eq!(rig.auth.security(true).failures_since_login, 1);
    rig.auth.limiter.lock().unwrap().reset();
    assert!(rig.auth.login(WHO, "first change".into()).await.is_ok());
}

#[tokio::test]
async fn removing_the_password_leaves_the_dashboard_open_for_good() {
    let mut rig = rig();
    assert_eq!(
        code(
            rig.auth
                .remove_password(WHO, String::new(), String::new())
                .await
                .unwrap_err()
        ),
        ErrorCode::FailedPrecondition
    );
    rig.with_password().await;
    let s = rig.auth.security(false);
    assert!(s.password_set && !s.offer_pending);
    let e = rig
        .auth
        .remove_password(WHO, "wrong".into(), String::new())
        .await
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::PermissionDenied);
    rig.auth.limiter.lock().unwrap().reset();
    rig.auth
        .remove_password(WHO, PASSWORD.into(), String::new())
        .await
        .unwrap();
    let s = rig.auth.security(false);
    assert!(!s.password_set && s.left_open && !s.offer_pending);
}

#[tokio::test]
async fn the_offer_stands_until_a_password_or_leave_it_open() {
    let rig = rig();
    assert!(rig.auth.security(false).offer_pending);
    rig.auth.leave_open().await.unwrap();
    let s = rig.auth.security(false);
    assert!(s.left_open && !s.offer_pending);
    // It is stored on the satellite, not in the browser.
    assert!(rig.service.dashboard().left_open);
}

#[tokio::test]
async fn a_password_and_leaving_it_open_exclude_each_other() {
    let mut rig = rig();
    rig.with_password().await;
    assert_eq!(
        code(rig.auth.leave_open().await.unwrap_err()),
        ErrorCode::FailedPrecondition
    );
}

#[tokio::test]
async fn a_token_is_shown_once_and_opens_only_its_own_door() {
    let rig = rig();
    let agent = rig.auth.create_token(TokenKind::Agent).await.unwrap();
    let metrics = rig.auth.create_token(TokenKind::Metrics).await.unwrap();
    let s = rig.auth.security(false);
    assert_eq!(s.agent_token_last4, agent[agent.len() - 4..]);
    assert_eq!(s.metrics_token_last4, metrics[metrics.len() - 4..]);
    let bearer = |t: &str| {
        let mut h = axum::http::HeaderMap::new();
        h.insert("authorization", format!("Bearer {t}").parse().unwrap());
        h
    };
    assert!(rig.auth.bearer_ok(TokenKind::Agent, &bearer(&agent)));
    assert!(!rig.auth.bearer_ok(TokenKind::Agent, &bearer(&metrics)));
    assert!(!rig.auth.bearer_ok(TokenKind::Metrics, &bearer(&agent)));
    // The file keeps a digest, never the token.
    let text = std::fs::read_to_string(rig.dir.path().join("secrets.toml")).unwrap();
    assert!(!text.contains(&agent) && !text.contains(&metrics), "{text}");

    let newer = rig.auth.create_token(TokenKind::Agent).await.unwrap();
    assert!(!rig.auth.bearer_ok(TokenKind::Agent, &bearer(&agent)));
    assert!(rig.auth.bearer_ok(TokenKind::Agent, &bearer(&newer)));
}

#[tokio::test]
async fn a_confirmation_waits_for_one_press_and_then_for_a_minute() {
    let mut rig = rig();
    let (id, seconds) = rig
        .auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap();
    assert_eq!(seconds, 60);
    let Some(Input::Presence(presence)) = rig.inputs.recv().await else {
        panic!("no presence");
    };
    // The ring waits for a minute.
    assert!(presence.until > Instant::now() + Duration::from_secs(58));

    // One at a time.
    let e = rig
        .auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::ResourceExhausted);
    assert_eq!(
        code(rig.auth.watch_confirmation("nope").unwrap_err()),
        ErrorCode::NotFound
    );

    let mut progress = rig.auth.watch_confirmation(&id).unwrap();
    assert_eq!(*progress.borrow(), ConfirmProgress::Waiting);
    presence.pressed.send(()).unwrap();
    progress.changed().await.unwrap();
    let ConfirmProgress::Confirmed(grant) = progress.borrow().clone() else {
        panic!("not confirmed");
    };
    assert_eq!(grant.len(), 43, "32 bytes, base64url");

    // A minute after it ended, not before.
    let s = rig.auth.security(false);
    assert!((59..=61).contains(&s.confirmation_cooldown_s), "{s:?}");
    let e = rig
        .auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::ResourceExhausted);
}

#[tokio::test]
async fn a_press_lifts_the_login_limit() {
    let mut rig = rig();
    rig.with_password().await;
    {
        let mut limiter = rig.auth.limiter.lock().unwrap();
        for _ in 0..10 {
            limiter.failed(WHO, SystemTime::now());
        }
        limiter.admit(Instant::now()).unwrap();
    }
    assert!(rig.auth.security(false).login_retry_after_s > 30, "slowed");
    rig.press().await;
    assert_eq!(rig.auth.security(false).login_retry_after_s, 0);
}

#[tokio::test]
async fn a_confirmation_nobody_presses_expires() {
    let mut rig = rig();
    rig.auth = Auth::waiting(
        rig.service.clone(),
        "02:00:00:aa:bb:cc",
        rig.tx.clone(),
        Duration::from_millis(100),
    );
    let (id, _) = rig
        .auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap();
    let mut progress = rig.auth.watch_confirmation(&id).unwrap();
    let Some(Input::Presence(presence)) = rig.inputs.recv().await else {
        panic!("no presence");
    };
    progress.changed().await.unwrap();
    assert_eq!(*progress.borrow(), ConfirmProgress::Expired);
    // The Brain's side of the press gave up with it.
    assert!(presence.pressed.is_closed());
    // No grant came out of it.
    let used = rig
        .auth
        .grants
        .lock()
        .unwrap()
        .consume("", Purpose::Password, None, Instant::now());
    assert!(!used);
}

#[tokio::test]
async fn without_a_session_the_security_state_says_only_what_signing_in_needs() {
    let mut rig = rig();
    rig.service.agent_seen("claude-code".into(), WHO);
    rig.auth.note_metrics_read();
    rig.with_password().await;
    rig.auth.create_token(TokenKind::Agent).await.unwrap();
    let _ = rig.auth.login(WHO, "wrong".into()).await;
    rig.auth.open_home_assistant_window().unwrap();
    let full = rig.auth.security(true);
    assert!(full.failures_since_login == 1 && !full.agent_token_last4.is_empty());
    assert!(!full.mcp_clients.is_empty() && full.home_assistant_window_left_s > 0);

    let s = rig.auth.security(false);
    assert!(s.password_set && !s.signed_in && !s.offer_pending);
    assert!(
        s.login_retry_after_s >= 1,
        "the form says how long to wait: {s:?}"
    );
    assert_eq!(s.failures_since_login, 0);
    assert!(s.last_failure_address.is_empty() && s.last_failure_unix_ms == 0);
    assert!(s.agent_token_last4.is_empty() && s.metrics_token_last4.is_empty());
    assert!(s.mcp_clients.is_empty() && !s.metrics_read);
    assert_eq!(
        (s.home_assistant_window_left_s, s.group_join_left_s),
        (0, 0)
    );
}

#[tokio::test]
async fn the_security_state_names_the_clients_and_scrapes_of_the_last_day() {
    let rig = rig();
    let s = rig.auth.security(false);
    assert!(s.mcp_clients.is_empty() && !s.metrics_read);
    rig.service.agent_seen("claude-code".into(), WHO);
    rig.service.agent_seen("claude-code".into(), WHO);
    rig.service.agent_seen("home-assistant".into(), WHO);
    rig.auth.note_metrics_read();
    let s = rig.auth.security(true);
    assert_eq!(s.mcp_clients, ["home-assistant", "claude-code"]);
    assert!(s.metrics_read && s.signed_in);
}

fn keyless_server() -> pinna_esphome::DeviceConfig {
    let mut cfg = pinna_esphome::DeviceConfig::new("t", [0; 6]);
    cfg.port = 0;
    cfg.bind = Ipv4Addr::LOCALHOST.into();
    cfg
}

/// Whether the server takes a connection: it stays open, or it is closed
/// right after accept.
async fn takes_connections(esp: &pinna_esphome::EsphomeHandle) -> bool {
    use tokio::io::AsyncReadExt;
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", esp.local_addr().port()))
        .await
        .unwrap();
    let mut byte = [0u8; 1];
    tokio::time::timeout(Duration::from_millis(200), stream.read(&mut byte))
        .await
        .is_err()
}

#[tokio::test]
async fn without_a_password_the_windows_are_refused_and_nothing_closes() {
    let rig = rig();
    let (esp, _commands) = pinna_esphome::serve(keyless_server()).await.unwrap();
    rig.auth.attach_esphome(esp.clone());
    assert!(takes_connections(&esp).await);
    let e = rig.auth.open_home_assistant_window().unwrap_err();
    assert_eq!(code(e), ErrorCode::FailedPrecondition);
    let e = rig.auth.allow_group_join().unwrap_err();
    assert_eq!(code(e), ErrorCode::FailedPrecondition);
    let security = rig.auth.security(false);
    assert_eq!(
        (
            security.home_assistant_window_left_s,
            security.group_join_left_s
        ),
        (0, 0)
    );
}

#[tokio::test]
async fn with_a_password_and_no_key_the_esphome_api_opens_only_in_its_window() {
    let mut rig = rig();
    let (esp, _commands) = pinna_esphome::serve(keyless_server()).await.unwrap();
    rig.auth.attach_esphome(esp.clone());
    assert!(takes_connections(&esp).await);

    // A connection that is open when the password is set ends.
    let mut open = tokio::net::TcpStream::connect(("127.0.0.1", esp.local_addr().port()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    rig.with_password().await;
    {
        use tokio::io::AsyncReadExt;
        let mut byte = [0u8; 1];
        let ended = tokio::time::timeout(Duration::from_secs(2), open.read(&mut byte)).await;
        assert!(matches!(ended, Ok(Ok(0)) | Ok(Err(_))), "{ended:?}");
    }
    assert!(!takes_connections(&esp).await);

    rig.auth.open_home_assistant_window().unwrap();
    let left = rig.auth.security(true).home_assistant_window_left_s;
    assert!((14 * 60..=15 * 60).contains(&left), "{left}");
    assert!(takes_connections(&esp).await);

    // Removing the password ends the window with it, and a new password
    // starts with the window shut.
    let grant = rig.press().await;
    rig.auth
        .remove_password(WHO, String::new(), grant)
        .await
        .unwrap();
    assert_eq!(rig.auth.security(true).home_assistant_window_left_s, 0);
    assert!(takes_connections(&esp).await);
    rig.with_password().await;
    assert!(!takes_connections(&esp).await);
}

#[tokio::test]
async fn a_key_makes_the_window_pointless() {
    let mut rig = rig();
    let mut cfg = keyless_server();
    cfg.encryption_key = Some([7; 32]);
    let (esp, _commands) = pinna_esphome::serve(cfg).await.unwrap();
    rig.auth.attach_esphome(esp.clone());
    rig.with_password().await;
    let e = rig.auth.open_home_assistant_window().unwrap_err();
    assert_eq!(code(e.clone()), ErrorCode::FailedPrecondition);
    assert!(
        e.message
            .is_some_and(|m| m.contains("added this satellite already"))
    );
    assert_eq!(rig.auth.security(true).home_assistant_window_left_s, 0);
}

#[tokio::test]
async fn joining_a_group_needs_the_window_once_a_password_is_set() {
    let mut rig = rig();
    assert!(!rig.auth.group_join_open());
    rig.with_password().await;
    assert!(!rig.auth.group_join_open());
    rig.auth.allow_group_join().unwrap();
    assert!(rig.auth.group_join_open());
    let left = rig.auth.security(true).group_join_left_s;
    assert!((9 * 60..=10 * 60).contains(&left), "{left}");
}

#[test]
fn a_window_that_ended_has_no_seconds_left() {
    let now = Instant::now();
    assert_eq!(left(None, now), 0);
    assert_eq!(left(Some(now + Duration::from_millis(1500)), now), 2);
    assert_eq!(left(Some(now), now), 0);
    assert_eq!(left(Some(now), now + Duration::from_secs(1)), 0);
}

#[tokio::test]
async fn a_second_asker_is_told_a_confirmation_from_another_browser_waits() {
    let rig = rig();
    rig.auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap();
    let e = rig
        .auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap_err();
    assert_eq!(code(e.clone()), ErrorCode::ResourceExhausted);
    let message = e.message.unwrap_or_default();
    assert!(
        message.starts_with(
            "A confirmation from another browser is waiting. Press Action only if you started it. Try again in "
        ) && message.ends_with(" s."),
        "{message}"
    );
}

#[tokio::test]
async fn a_signed_in_caller_sees_who_waits_and_for_how_long() {
    let mut rig = rig();
    rig.with_password().await;
    let s = rig.auth.security(true);
    assert!(s.confirmation_waiting_from.is_empty() && s.confirmation_left_s == 0);
    rig.auth.confirmations.lock().unwrap().current = None;
    rig.auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap();
    let s = rig.auth.security(true);
    assert_eq!(s.confirmation_waiting_from, "192.168.1.9");
    assert!((59..=60).contains(&s.confirmation_left_s), "{s:?}");
    // Without a session, only the cooldown is told.
    let s = rig.auth.security(false);
    assert!(s.confirmation_waiting_from.is_empty() && s.confirmation_left_s == 0);
    assert!(s.confirmation_cooldown_s > 0);
}

#[tokio::test]
async fn the_id_cancels_a_confirmation_without_a_session() {
    let mut rig = rig();
    let (id, _) = rig
        .auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap();
    let mut progress = rig.auth.watch_confirmation(&id).unwrap();
    let Some(Input::Presence(mut presence)) = rig.inputs.recv().await else {
        panic!("no presence");
    };
    rig.auth.cancel_confirmation(&id, false).unwrap();
    assert_eq!(*progress.borrow_and_update(), ConfirmProgress::Canceled);
    // The Brain's side of the press is gone, so the ring stops.
    tokio::time::timeout(Duration::from_secs(2), presence.pressed.closed())
        .await
        .expect("the wait ended");
    let s = rig.auth.security(true);
    assert!(s.confirmation_waiting_from.is_empty() && s.confirmation_left_s == 0);
    // Ending it again is not an error.
    rig.auth.cancel_confirmation(&id, false).unwrap();
}

#[tokio::test]
async fn a_canceled_confirmation_gives_the_next_one_ten_seconds_not_a_minute() {
    let mut rig = rig();
    let (id, _) = rig
        .auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap();
    rig.auth.cancel_confirmation(&id, false).unwrap();
    let wait = rig.auth.security(false).confirmation_cooldown_s;
    assert!((9..=10).contains(&wait), "{wait}");
    let e = rig
        .auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::ResourceExhausted);
    // Ten seconds after it ended, the next may start.
    let ended = Instant::now() - Duration::from_secs(10);
    {
        let mut confirmations = rig.auth.confirmations.lock().unwrap();
        confirmations.current.as_mut().unwrap().ended = Some(ended);
    }
    rig.auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap();
    assert!(matches!(rig.inputs.recv().await, Some(Input::Presence(_))));
}

#[tokio::test]
async fn an_empty_id_cancels_whichever_waits_but_only_for_a_session() {
    let mut rig = rig();
    let (id, _) = rig
        .auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap();
    let progress = rig.auth.watch_confirmation(&id).unwrap();
    let e = rig.auth.cancel_confirmation("", false).unwrap_err();
    assert_eq!(code(e), ErrorCode::Unauthenticated);
    assert_eq!(*progress.borrow(), ConfirmProgress::Waiting);
    rig.auth.cancel_confirmation("", true).unwrap();
    assert_eq!(*progress.borrow(), ConfirmProgress::Canceled);
    // Nothing waits now: a second click finds it done.
    rig.auth.cancel_confirmation("", true).unwrap();
    let Some(Input::Presence(mut presence)) = rig.inputs.recv().await else {
        panic!("no presence");
    };
    tokio::time::timeout(Duration::from_secs(2), presence.pressed.closed())
        .await
        .expect("the wait ended");
}

#[tokio::test]
async fn an_unknown_id_cancels_nothing_and_a_press_is_not_undone() {
    let mut rig = rig();
    let (id, _) = rig
        .auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap();
    let e = rig.auth.cancel_confirmation("nope", true).unwrap_err();
    assert_eq!(code(e), ErrorCode::NotFound);
    let mut progress = rig.auth.watch_confirmation(&id).unwrap();
    let Some(Input::Presence(presence)) = rig.inputs.recv().await else {
        panic!("no presence");
    };
    presence.pressed.send(()).unwrap();
    progress.changed().await.unwrap();
    assert!(matches!(*progress.borrow(), ConfirmProgress::Confirmed(_)));
    rig.auth.cancel_confirmation(&id, false).unwrap();
    assert!(matches!(*progress.borrow(), ConfirmProgress::Confirmed(_)));
}

#[tokio::test]
async fn a_recording_confirmation_needs_a_password_and_a_session() {
    let mut rig = rig();
    let e = rig
        .auth
        .start_confirmation(WHO, Purpose::Recording, Some("a session"))
        .unwrap_err();
    assert_eq!(code(e.clone()), ErrorCode::FailedPrecondition);
    assert_eq!(
        e.message.as_deref(),
        Some("Recording needs a dashboard password. Set one under System > Password and tokens.")
    );
    rig.with_password().await;
    rig.auth.confirmations.lock().unwrap().current = None;
    let e = rig
        .auth
        .start_confirmation(WHO, Purpose::Recording, None)
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::Unauthenticated);
    // Neither refusal rang the ring.
    assert!(rig.inputs.try_recv().is_err());
    rig.auth
        .start_confirmation(WHO, Purpose::Recording, Some("a session"))
        .unwrap();
}

#[tokio::test]
async fn the_purpose_reaches_the_ring_and_the_signed_in_overview() {
    let mut rig = rig();
    rig.with_password().await;
    rig.auth.confirmations.lock().unwrap().current = None;
    rig.auth
        .start_confirmation(WHO, Purpose::Recording, Some("mine"))
        .unwrap();
    let Some(Input::Presence(presence)) = rig.inputs.recv().await else {
        panic!("no presence");
    };
    assert_eq!(presence.purpose, Purpose::Recording);
    let s = rig.auth.security(true);
    assert_eq!(
        s.confirmation_purpose.as_known(),
        Some(ConfirmationPurpose::CONFIRMATION_PURPOSE_RECORDING)
    );
    // Without a session the purpose isn't told either.
    let s = rig.auth.security(false);
    assert_eq!(
        s.confirmation_purpose.as_known(),
        Some(ConfirmationPurpose::CONFIRMATION_PURPOSE_PASSWORD)
    );
    // A password confirmation says so.
    rig.auth.confirmations.lock().unwrap().current = None;
    rig.auth
        .start_confirmation(WHO, Purpose::Password, None)
        .unwrap();
    let s = rig.auth.security(true);
    assert_eq!(
        s.confirmation_purpose.as_known(),
        Some(ConfirmationPurpose::CONFIRMATION_PURPOSE_PASSWORD)
    );
}

#[tokio::test]
async fn a_recording_grant_is_not_a_password_grant_and_the_other_way_round() {
    let mut rig = rig();
    rig.with_password().await;

    let recording = rig.press_for(Purpose::Recording, Some("mine")).await;
    // It changes nothing about the password.
    let e = rig
        .auth
        .set_password(WHO, "another one".into(), String::new(), recording.clone())
        .await
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::PermissionDenied);
    let e = rig
        .auth
        .remove_password(WHO, String::new(), recording.clone())
        .await
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::PermissionDenied);
    assert!(rig.auth.security(false).password_set);
    // Those refusals left it good for a recording.
    rig.auth
        .take_recording_grant(&recording, Some("mine"))
        .unwrap();

    // A password grant doesn't start a recording.
    let password = rig.press().await;
    let e = rig
        .auth
        .take_recording_grant(&password, Some("mine"))
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn a_recording_grant_belongs_to_the_session_that_asked_and_works_once() {
    let mut rig = rig();
    rig.with_password().await;
    let grant = rig.press_for(Purpose::Recording, Some("mine")).await;
    let e = rig
        .auth
        .take_recording_grant(&grant, Some("theirs"))
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::PermissionDenied);
    let e = rig.auth.take_recording_grant(&grant, None).unwrap_err();
    assert_eq!(code(e), ErrorCode::Unauthenticated);
    rig.auth.take_recording_grant(&grant, Some("mine")).unwrap();
    let e = rig
        .auth
        .take_recording_grant(&grant, Some("mine"))
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn recording_needs_a_password_when_the_grant_is_presented_too() {
    let rig = rig();
    let e = rig
        .auth
        .take_recording_grant("anything", Some("mine"))
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::FailedPrecondition);
}

#[tokio::test]
async fn a_debugging_text_grant_needs_a_password_its_session_and_ten_seconds() {
    let mut rig = rig();
    let e = rig
        .auth
        .take_debugging_text_grant("anything", Some("mine"))
        .unwrap_err();
    assert_eq!(code(e.clone()), ErrorCode::FailedPrecondition);
    assert_eq!(
        e.message.as_deref(),
        Some("Set a dashboard password to record what was said.")
    );
    let e = rig
        .auth
        .start_confirmation(WHO, Purpose::DebuggingText, Some("mine"))
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::FailedPrecondition);

    rig.with_password().await;
    rig.auth.confirmations.lock().unwrap().current = None;
    let e = rig
        .auth
        .start_confirmation(WHO, Purpose::DebuggingText, None)
        .unwrap_err();
    assert_eq!(code(e.clone()), ErrorCode::Unauthenticated);
    assert_eq!(
        e.message.as_deref(),
        Some("Sign in to record what was said.")
    );

    let grant = rig.press_for(Purpose::DebuggingText, Some("mine")).await;
    let e = rig
        .auth
        .take_debugging_text_grant(&grant, Some("theirs"))
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::PermissionDenied);
    // A recording doesn't take it.
    let e = rig
        .auth
        .take_recording_grant(&grant, Some("mine"))
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::PermissionDenied);
    rig.auth
        .take_debugging_text_grant(&grant, Some("mine"))
        .unwrap();
    let e = rig
        .auth
        .take_debugging_text_grant(&grant, Some("mine"))
        .unwrap_err();
    assert_eq!(code(e), ErrorCode::PermissionDenied);
}
