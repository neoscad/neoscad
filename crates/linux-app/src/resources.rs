//! The editor's page, served offline under its own URI scheme, as the
//! macOS app serves it (`apple/App/Editor/EditorSchemeHandler.swift`):
//! `neoscad-editor://app/editor.html` and the files beside it, with a
//! Content-Security-Policy whose style nonce is fresh for each load. The
//! page's own `<meta>` repeats the policy (`apple/Editor/web/src/editor.html`)
//! so it holds even where a response's headers are lost.
//!
//! Where the bundle is: `scripts/apple/build-editor.sh` writes it to
//! `apple/Editor/web/dist`; an installed app finds it under
//! `share/neoscad/editor` beside its `bin`.

use std::path::{Path, PathBuf};

/// The editor page's URI scheme (the macOS app's).
pub const SCHEME: &str = "neoscad-editor";

/// The page the web view loads.
pub const PAGE_URL: &str = "neoscad-editor://app/editor.html";

/// The Content-Security-Policy of the page: nothing from the network, and
/// the only script is the bundle beside the page.
pub fn content_security_policy(nonce: &str) -> String {
    [
        "default-src 'none'".to_string(),
        format!("script-src {SCHEME}:"),
        format!("style-src 'nonce-{nonce}'"),
        "img-src data:".into(),
        "base-uri 'none'".into(),
        "form-action 'none'".into(),
    ]
    .join("; ")
}

/// One response of the scheme handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub body: Vec<u8>,
    pub content_type: &'static str,
    /// The CSP header, for the page.
    pub csp: Option<String>,
}

/// The resource for `uri` from the bundle in `dir`, or `None` (a 404).
/// Only plain file names directly in the bundle are served: no
/// subdirectories, no dot files, no other host, so a script cannot read
/// anything else from the disk through the scheme.
pub fn resolve(dir: &Path, uri: &str, nonce: &str) -> Option<Resource> {
    let rest = uri.strip_prefix(SCHEME)?.strip_prefix("://app/")?;
    let name = rest.split(['?', '#']).next()?;
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return None;
    }
    let content_type = match Path::new(name).extension()?.to_str()? {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        _ => return None,
    };
    let mut body = std::fs::read(dir.join(name)).ok()?;
    let mut csp = None;
    if content_type.starts_with("text/html") {
        let html = String::from_utf8_lossy(&body).replace("NONCE_PLACEHOLDER", nonce);
        body = html.into_bytes();
        csp = Some(content_security_policy(nonce));
    }
    Some(Resource {
        body,
        content_type,
        csp,
    })
}

/// Where to look for the bundle, first match wins: `$NEOSCAD_EDITOR_DIR`,
/// then `../share/neoscad/editor` and `editor` beside the executable (an
/// install, a tarball), then the checkout's build output when the binary
/// is run from `target/` (development).
pub fn editor_dir_candidates(env_dir: Option<PathBuf>, exe: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    out.extend(env_dir);
    if let Some(bin) = exe.and_then(Path::parent) {
        out.push(bin.join("../share/neoscad/editor"));
        out.push(bin.join("editor"));
        // target/<profile>/neoscad-gtk -> <checkout>/apple/Editor/web/dist
        // (also target/<dir>/<profile>/ for a named CARGO_TARGET_DIR).
        for up in [2, 3] {
            if let Some(root) = bin.ancestors().nth(up) {
                out.push(root.join("apple/Editor/web/dist"));
            }
        }
    }
    out
}

/// The first candidate holding `editor.html`.
pub fn find_editor_dir(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|d| d.join("editor.html").is_file())
        .cloned()
}

/// A nonce for one page load: 128 bits from the kernel, hex. Falls back
/// to the process id and address-space randomness if `/dev/urandom` is
/// unreadable (a CSP nonce only has to be unguessable by the page itself,
/// which has no way to read the header before it runs).
pub fn nonce() -> String {
    use std::io::Read;
    let mut b = [0u8; 16];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_ok();
    if !ok {
        let seed = std::process::id() as usize ^ (&b as *const _ as usize);
        b[..8].copy_from_slice(&(seed as u64).to_le_bytes());
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("neoscad-linux-app-res-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("editor.html"),
            "<meta content=\"style-src 'nonce-NONCE_PLACEHOLDER'\">",
        )
        .unwrap();
        std::fs::write(dir.join("editor.js"), "1;").unwrap();
        std::fs::write(dir.join(".secret.js"), "x").unwrap();
        dir
    }

    #[test]
    fn serves_the_page_with_a_fresh_nonce_and_its_policy() {
        let dir = bundle();
        let r = resolve(&dir, PAGE_URL, "abc").unwrap();
        assert_eq!(r.content_type, "text/html; charset=utf-8");
        assert_eq!(
            String::from_utf8(r.body).unwrap(),
            "<meta content=\"style-src 'nonce-abc'\">"
        );
        let csp = r.csp.unwrap();
        assert!(csp.contains("style-src 'nonce-abc'"), "{csp}");
        assert!(csp.contains("script-src neoscad-editor:"), "{csp}");
        let js = resolve(&dir, "neoscad-editor://app/editor.js?v=1", "abc").unwrap();
        assert_eq!(js.body, b"1;");
        assert_eq!(js.csp, None);
    }

    #[test]
    fn refuses_anything_outside_the_bundle() {
        let dir = bundle();
        for uri in [
            "neoscad-editor://app/../editor.html",
            "neoscad-editor://app/sub/editor.js",
            "neoscad-editor://app/.secret.js",
            "neoscad-editor://other/editor.js",
            "file:///etc/passwd",
            "neoscad-editor://app/missing.js",
            "neoscad-editor://app/editor.png",
            "neoscad-editor://app/",
        ] {
            assert_eq!(resolve(&dir, uri, "n"), None, "{uri}");
        }
    }

    #[test]
    fn looks_beside_the_binary_and_in_the_checkout() {
        let c = editor_dir_candidates(
            Some(PathBuf::from("/opt/editor")),
            Some(Path::new("/w/target/wt/debug/neoscad-gtk")),
        );
        assert_eq!(c[0], PathBuf::from("/opt/editor"));
        assert!(c.contains(&PathBuf::from("/w/target/wt/debug/../share/neoscad/editor")));
        assert!(c.contains(&PathBuf::from("/w/apple/Editor/web/dist")));
        assert!(c.contains(&PathBuf::from("/w/target/apple/Editor/web/dist")));
        assert_eq!(find_editor_dir(&[bundle()]), Some(bundle()));
        assert_eq!(find_editor_dir(&[PathBuf::from("/nonexistent")]), None);
    }

    #[test]
    fn nonces_are_hex_and_differ() {
        let (a, b) = (nonce(), nonce());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
