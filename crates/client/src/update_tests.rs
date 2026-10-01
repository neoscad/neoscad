//! The update check against feeds that `scripts/release/update-feed.py`
//! wrote and minisign signed with a throwaway test key
//! (`testdata/update/make.sh` regenerates them).

use super::*;

macro_rules! data {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/update/",
            $name
        ))
    };
}

/// test.pub's key line.
fn test_key() -> &'static str {
    let text = std::str::from_utf8(data!("test.pub")).unwrap();
    text.lines().nth(1).unwrap()
}

// 0.3.0 before its DMG landed (serial 1), and after (serial 2).
const STABLE_1: &[u8] = data!("stable-1.json");
const STABLE_1_SIG: &[u8] = data!("stable-1.json.minisig");
const STABLE_2: &[u8] = data!("stable-2.json");
const STABLE_2_SIG: &[u8] = data!("stable-2.json.minisig");
// The same feed signed by a key nobody trusts.
const STABLE_2_OTHER_SIG: &[u8] = data!("stable-2.json.other.minisig");
// 0.4.0-rc.1, with only the Linux x86_64 Flatpak.
const RC_1: &[u8] = data!("rc-1.json");
const RC_1_SIG: &[u8] = data!("rc-1.json.minisig");

fn check_test(
    feed: &[u8],
    sig: &[u8],
    current: &str,
    channel: Channel,
    platform: Option<Platform>,
    last_serial: Option<u64>,
) -> Result<Checked, UpdateError> {
    check_with_keys(
        &[test_key()],
        feed,
        sig,
        current,
        channel,
        platform,
        last_serial,
    )
}

#[test]
fn offers_a_newer_release_to_the_cli() {
    let c = check_test(STABLE_2, STABLE_2_SIG, "0.2.0", Channel::Stable, None, None).unwrap();
    assert_eq!(c.serial, 2);
    assert_eq!(c.version, "0.3.0");
    let u = c.update.expect("0.3.0 > 0.2.0");
    assert_eq!(u.version, "0.3.0");
    assert_eq!(u.date, "2026-10-01");
    assert_eq!(
        u.url,
        "https://github.com/neoscad/neoscad/releases/tag/v0.3.0"
    );
    assert_eq!(u.artifact, None, "the CLI asks about no platform");
}

#[test]
fn offers_an_app_its_own_installer() {
    let c = check_test(
        STABLE_2,
        STABLE_2_SIG,
        "0.2.0",
        Channel::Stable,
        Some(Platform::WindowsArm64),
        Some(1),
    )
    .unwrap();
    let a = c.update.unwrap().artifact.unwrap();
    assert_eq!(a.name, "NeoSCAD-0.3.0-windows-arm64.msi");
    assert_eq!(a.sha256.len(), 64);
    assert!(a.url.ends_with("/v0.3.0/NeoSCAD-0.3.0-windows-arm64.msi"));
    assert_eq!(a.size, 1002);
}

#[test]
fn a_mac_waits_for_its_dmg() {
    // Serial 1 was published before the DMG was notarized: Windows and
    // Linux are offered 0.3.0, the Mac is not, and the CLI is.
    let mac = check_test(
        STABLE_1,
        STABLE_1_SIG,
        "0.2.0",
        Channel::Stable,
        Some(Platform::MacOs),
        None,
    )
    .unwrap();
    assert_eq!(mac.update, None);
    assert_eq!(mac.serial, 1);
    let linux = check_test(
        STABLE_1,
        STABLE_1_SIG,
        "0.2.0",
        Channel::Stable,
        Some(Platform::LinuxX86_64),
        None,
    )
    .unwrap();
    assert!(linux.update.is_some());
    let cli = check_test(STABLE_1, STABLE_1_SIG, "0.2.0", Channel::Stable, None, None).unwrap();
    assert!(cli.update.is_some());
    // Once it lands (serial 2), the Mac gets it.
    let mac = check_test(
        STABLE_2,
        STABLE_2_SIG,
        "0.2.0",
        Channel::Stable,
        Some(Platform::MacOs),
        Some(1),
    )
    .unwrap();
    assert_eq!(
        mac.update.unwrap().artifact.unwrap().name,
        "NeoSCAD-0.3.0-1234.dmg"
    );
}

#[test]
fn never_offers_the_same_or_an_older_version() {
    for current in ["0.3.0", "v0.3.0", "0.3.1", "1.0.0", "0.4.0-rc.1"] {
        let c = check_test(STABLE_2, STABLE_2_SIG, current, Channel::Stable, None, None).unwrap();
        assert_eq!(c.update, None, "running {current}");
    }
    // A release candidate of 0.3.0 is older than 0.3.0 itself.
    let c = check_test(
        STABLE_2,
        STABLE_2_SIG,
        "0.3.0-rc.2",
        Channel::Stable,
        None,
        None,
    )
    .unwrap();
    assert!(c.update.is_some());
}

#[test]
fn refuses_a_replayed_older_feed() {
    // Having seen serial 2, an attacker (or a stale cache) serving the
    // validly signed serial 1 is refused, not believed.
    let e = check_test(
        STABLE_1,
        STABLE_1_SIG,
        "0.2.0",
        Channel::Stable,
        None,
        Some(2),
    )
    .unwrap_err();
    assert_eq!(e, UpdateError::Replay { last: 2, found: 1 });
    // The same serial again is the same feed fetched twice: fine.
    assert!(
        check_test(
            STABLE_2,
            STABLE_2_SIG,
            "0.2.0",
            Channel::Stable,
            None,
            Some(2)
        )
        .is_ok()
    );
}

#[test]
fn rc_channel() {
    let c = check_test(
        RC_1,
        RC_1_SIG,
        "0.3.0",
        Channel::Rc,
        Some(Platform::LinuxX86_64),
        None,
    )
    .unwrap();
    assert_eq!(c.update.unwrap().version, "0.4.0-rc.1");
    // Its only installer is Linux x86_64.
    let c = check_test(
        RC_1,
        RC_1_SIG,
        "0.3.0",
        Channel::Rc,
        Some(Platform::MacOs),
        None,
    )
    .unwrap();
    assert_eq!(c.update, None);
}

#[test]
fn refuses_a_feed_for_the_other_channel() {
    // A signed rc.json served at the stable URL must not move stable users
    // onto a release candidate.
    let e = check_test(RC_1, RC_1_SIG, "0.2.0", Channel::Stable, None, None).unwrap_err();
    assert_eq!(
        e,
        UpdateError::WrongChannel {
            expected: Channel::Stable,
            found: Channel::Rc
        }
    );
    assert!(check_test(STABLE_2, STABLE_2_SIG, "0.2.0", Channel::Rc, None, None).is_err());
}

#[test]
fn refuses_tampered_or_untrusted_feeds() {
    // One byte changed: the version.
    let tampered = String::from_utf8(STABLE_2.to_vec())
        .unwrap()
        .replace("\"version\": \"0.3.0\"", "\"version\": \"9.0.0\"");
    assert_ne!(tampered.as_bytes(), STABLE_2);
    let e = check_test(
        tampered.as_bytes(),
        STABLE_2_SIG,
        "0.2.0",
        Channel::Stable,
        None,
        None,
    );
    assert_eq!(e.unwrap_err(), UpdateError::BadSignature);
    // Another feed's signature.
    let e = check_test(STABLE_2, STABLE_1_SIG, "0.2.0", Channel::Stable, None, None);
    assert_eq!(e.unwrap_err(), UpdateError::BadSignature);
    // Signed by a key that isn't trusted.
    let e = check_test(
        STABLE_2,
        STABLE_2_OTHER_SIG,
        "0.2.0",
        Channel::Stable,
        None,
        None,
    );
    assert_eq!(e.unwrap_err(), UpdateError::BadSignature);
    // Not a signature at all.
    let e = check_test(STABLE_2, b"hello", "0.2.0", Channel::Stable, None, None);
    assert_eq!(e.unwrap_err(), UpdateError::BadSignatureFile);
    // No keys: nothing can be trusted.
    let e = check_with_keys(
        &[],
        STABLE_2,
        STABLE_2_SIG,
        "0.2.0",
        Channel::Stable,
        None,
        None,
    );
    assert_eq!(e.unwrap_err(), UpdateError::NoTrustedKeys);
}

#[test]
fn any_listed_key_will_do() {
    // Rotation: a build trusts the old and the new key at once; a broken
    // entry in the list doesn't stop the others from being tried.
    let keys = [
        "not a key",
        "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3",
        test_key(),
    ];
    let c = check_with_keys(
        &keys,
        STABLE_2,
        STABLE_2_SIG,
        "0.2.0",
        Channel::Stable,
        None,
        None,
    );
    assert!(c.unwrap().update.is_some());
}

#[test]
fn bad_versions_are_errors() {
    let e = check_test(STABLE_2, STABLE_2_SIG, "dev", Channel::Stable, None, None).unwrap_err();
    assert_eq!(e, UpdateError::BadVersion("dev".into()));
}

#[test]
fn release_builds_trust_only_release_keys() {
    // The test key must never be compiled in as a release key.
    assert!(!RELEASE_KEYS.contains(&test_key()));
    for k in RELEASE_KEYS {
        assert!(
            minisign_verify::PublicKey::from_base64(k).is_ok(),
            "{k} is not a minisign public key"
        );
    }
}

#[test]
fn platform_keys_round_trip() {
    for p in [
        Platform::MacOs,
        Platform::WindowsX64,
        Platform::WindowsArm64,
        Platform::LinuxX86_64,
        Platform::LinuxAarch64,
    ] {
        assert_eq!(Platform::from_key(p.key()), Some(p));
    }
    assert_eq!(
        Channel::Rc.feed_url(FEED_BASE_URL),
        "https://neoscad.org/updates/v1/rc.json"
    );
}
