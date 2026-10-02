//! The update check against the signed fixtures in
//! `crates/client/testdata/update` (a throwaway key; `make.sh` there).

use super::*;

macro_rules! data {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../client/testdata/update/",
            $name
        ))
    };
}

/// The throwaway key `crates/client/testdata/update` is signed with.
fn test_key() -> &'static str {
    std::str::from_utf8(data!("test.pub"))
        .unwrap()
        .lines()
        .nth(1)
        .unwrap()
}

// 0.3.0 with every installer (serial 2); signed by test.key, and by a
// key nobody trusts. rc-1 is 0.4.0-rc.1 with only the x86_64 bundle.
const STABLE_2: &[u8] = data!("stable-2.json");
const STABLE_2_SIG: &[u8] = data!("stable-2.json.minisig");
const STABLE_2_OTHER_SIG: &[u8] = data!("stable-2.json.other.minisig");
const STABLE_1: &[u8] = data!("stable-1.json");
const STABLE_1_SIG: &[u8] = data!("stable-1.json.minisig");
const RC_1: &[u8] = data!("rc-1.json");
const RC_1_SIG: &[u8] = data!("rc-1.json.minisig");

fn eval(
    s: &mut Settings,
    feed: &[u8],
    sig: &[u8],
    current: &str,
    install: Install,
    arch: &str,
) -> Result<Outcome, UpdateError> {
    evaluate(s, &[test_key()], feed, sig, current, install, arch)
}

fn notice(o: Outcome) -> Notice {
    match o {
        Outcome::Available(n) => n,
        o => panic!("expected a notice, got {o:?}"),
    }
}

#[test]
fn defaults_follow_the_owner_decisions() {
    let s = Settings::default();
    assert!(s.automatic, "automatic checks are on by default");
    assert!(!s.rc, "release candidates are opt-in");
    assert_eq!(s.channel(), Channel::Stable);
    assert!(s.due(1_000_000));
}

#[test]
fn settings_round_trip_and_survive_damage() {
    let s = Settings {
        automatic: false,
        rc: true,
        checked: 42,
        stable_serial: Some(3),
        rc_serial: None,
        dismissed: Some("0.3.0".into()),
    };
    assert_eq!(Settings::from_json(s.to_json().as_bytes()), s);
    assert_eq!(Settings::from_json(b"not json"), Settings::default());
    assert_eq!(Settings::from_json(b"[1]"), Settings::default());
    let partial = Settings::from_json(br#"{"rc": true, "automatic": "yes"}"#);
    assert!(partial.rc && partial.automatic);
}

#[test]
fn settings_save_and_load() {
    let dir = std::env::temp_dir().join(format!("neoscad-update-{}", std::process::id()));
    let path = settings_path(&dir);
    assert_eq!(Settings::load(&path), Settings::default());
    let s = Settings {
        rc: true,
        checked: 7,
        ..Settings::default()
    };
    s.save(&path).unwrap();
    assert_eq!(Settings::load(&path), s);
    assert!(path.ends_with("neoscad/updates.json"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_check_is_due_once_a_day() {
    let mut s = Settings {
        checked: 1_000_000,
        ..Settings::default()
    };
    assert!(!s.due(1_000_000 + INTERVAL_SECS - 1));
    assert!(s.due(1_000_000 + INTERVAL_SECS));
    assert!(s.due(999_999), "the clock went back");
    s.automatic = false;
    assert!(!s.due(1_000_000 + 10 * INTERVAL_SECS));
}

#[test]
fn the_feed_base_is_https_or_loopback() {
    assert_eq!(feed_base(None).unwrap(), update::FEED_BASE_URL);
    assert_eq!(feed_base(Some("  ")).unwrap(), update::FEED_BASE_URL);
    assert_eq!(
        feed_base(Some("http://127.0.0.1:8000/updates/v1")).unwrap(),
        "http://127.0.0.1:8000/updates/v1/"
    );
    assert!(feed_base(Some("https://example.org/u/")).is_ok());
    assert!(feed_base(Some("http://example.org/u/")).is_err());
    assert!(feed_base(Some("file:///tmp/")).is_err());
    let (f, s) = feed_urls("https://neoscad.org/updates/v1/", Channel::Rc);
    assert_eq!(f, "https://neoscad.org/updates/v1/rc.json");
    assert_eq!(s, "https://neoscad.org/updates/v1/rc.json.minisig");
}

#[test]
fn a_flatpak_is_offered_its_bundle() {
    let mut s = Settings::default();
    let n = notice(
        eval(
            &mut s,
            STABLE_2,
            STABLE_2_SIG,
            "0.2.1",
            Install::Flatpak,
            "x86_64",
        )
        .unwrap(),
    );
    assert_eq!(n.title(), "NeoSCAD 0.3.0 is available");
    let b = n.bundle.as_ref().unwrap();
    assert_eq!(b.name, "NeoSCAD-0.3.0-linux-x86_64.flatpak");
    assert_eq!(n.download_url(), b.url);
    assert_eq!(n.download_label(), "Download Bundle");
    assert!(
        n.body()
            .contains("flatpak install --user --reinstall NeoSCAD-0.3.0-linux-x86_64.flatpak")
    );
    assert_eq!(s.stable_serial, Some(2), "the serial is kept");
    let n = notice(
        eval(
            &mut s,
            STABLE_2,
            STABLE_2_SIG,
            "0.2.1",
            Install::Flatpak,
            "aarch64",
        )
        .unwrap(),
    );
    assert_eq!(
        n.bundle.unwrap().name,
        "NeoSCAD-0.3.0-linux-aarch64.flatpak"
    );
}

#[test]
fn another_install_is_sent_to_the_release_page() {
    let mut s = Settings::default();
    let n = notice(
        eval(
            &mut s,
            STABLE_2,
            STABLE_2_SIG,
            "0.2.1",
            Install::Other,
            "riscv64",
        )
        .unwrap(),
    );
    assert_eq!(n.bundle, None);
    assert_eq!(
        n.download_url(),
        "https://github.com/neoscad/neoscad/releases/tag/v0.3.0"
    );
    assert_eq!(n.download_label(), "Open Release Page");
    assert!(n.body().contains("pull and build again"));
}

#[test]
fn a_bad_signature_is_refused_and_changes_nothing() {
    let mut s = Settings::default();
    let before = s.clone();
    assert_eq!(
        eval(
            &mut s,
            STABLE_2,
            STABLE_2_OTHER_SIG,
            "0.2.1",
            Install::Flatpak,
            "x86_64"
        ),
        Err(UpdateError::BadSignature)
    );
    // A feed edited after signing.
    let tampered = String::from_utf8(STABLE_2.to_vec())
        .unwrap()
        .replace("0.3.0", "9.9.9");
    assert_eq!(
        eval(
            &mut s,
            tampered.as_bytes(),
            STABLE_2_SIG,
            "0.2.1",
            Install::Flatpak,
            "x86_64"
        ),
        Err(UpdateError::BadSignature)
    );
    // No key at all, as in a build before the release key exists.
    assert_eq!(
        evaluate(
            &mut s,
            &[],
            STABLE_2,
            STABLE_2_SIG,
            "0.2.1",
            Install::Other,
            "x86_64"
        ),
        Err(UpdateError::NoTrustedKeys)
    );
    assert_eq!(s, before);
}

#[test]
fn an_old_feed_is_not_replayed() {
    let mut s = Settings::default();
    eval(
        &mut s,
        STABLE_2,
        STABLE_2_SIG,
        "0.2.1",
        Install::Other,
        "x86_64",
    )
    .unwrap();
    assert_eq!(
        eval(
            &mut s,
            STABLE_1,
            STABLE_1_SIG,
            "0.2.1",
            Install::Other,
            "x86_64"
        ),
        Err(UpdateError::Replay { last: 2, found: 1 })
    );
}

#[test]
fn up_to_date_when_running_the_latest() {
    let mut s = Settings::default();
    assert_eq!(
        eval(
            &mut s,
            STABLE_2,
            STABLE_2_SIG,
            "0.3.0",
            Install::Flatpak,
            "x86_64"
        )
        .unwrap(),
        Outcome::UpToDate {
            latest: "0.3.0".into()
        }
    );
}

#[test]
fn release_candidates_only_on_the_rc_channel() {
    // The stable channel refuses the rc feed outright.
    let mut s = Settings::default();
    assert!(matches!(
        eval(&mut s, RC_1, RC_1_SIG, "0.3.0", Install::Flatpak, "x86_64"),
        Err(UpdateError::WrongChannel { .. })
    ));
    // Opted in: 0.4.0-rc.1, kept apart from the stable serial.
    s.rc = true;
    let n = notice(eval(&mut s, RC_1, RC_1_SIG, "0.3.0", Install::Flatpak, "x86_64").unwrap());
    assert_eq!(n.version, "0.4.0-rc.1");
    assert_eq!(s.rc_serial, Some(1));
    assert_eq!(s.stable_serial, None);
    // The rc has no aarch64 bundle, so an aarch64 Flatpak waits.
    assert!(matches!(
        eval(&mut s, RC_1, RC_1_SIG, "0.3.0", Install::Flatpak, "aarch64").unwrap(),
        Outcome::UpToDate { .. }
    ));
}

#[test]
fn later_hides_only_that_version_from_automatic_checks() {
    let mut s = Settings::default();
    let n = notice(
        eval(
            &mut s,
            STABLE_2,
            STABLE_2_SIG,
            "0.2.1",
            Install::Other,
            "x86_64",
        )
        .unwrap(),
    );
    assert!(should_show(&n, &s, false));
    s.dismissed = Some("0.3.0".into());
    assert!(!should_show(&n, &s, false));
    assert!(
        should_show(&n, &s, true),
        "the menu's check always shows it"
    );
    s.dismissed = Some("0.2.9".into());
    assert!(should_show(&n, &s, false));
}

#[test]
fn install_detection_and_platforms() {
    assert_eq!(Install::detect(true), Install::Flatpak);
    assert_eq!(Install::detect(false), Install::Other);
    assert_eq!(
        Install::Flatpak.platform("x86_64"),
        Some(Platform::LinuxX86_64)
    );
    assert_eq!(
        Install::Flatpak.platform("aarch64"),
        Some(Platform::LinuxAarch64)
    );
    assert_eq!(Install::Flatpak.platform("riscv64"), None);
    assert_eq!(Install::Other.platform("x86_64"), None);
}
