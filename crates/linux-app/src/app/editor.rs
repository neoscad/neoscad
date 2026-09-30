//! The editor: the macOS app's CodeMirror bundle in a WebKitGTK 6 web
//! view, spoken to with the same protocol (`linux_app::bridge`).
//!
//! JS -> host: the page posts to `window.webkit.messageHandlers.editor`,
//! registered here "with reply" because the page treats `postMessage` as
//! returning a promise (`apple/Editor/web/src/bridge.js`, `editorHost`),
//! as WKWebView's `WKScriptMessageHandlerWithReply` does. Host -> JS:
//! `call_async_javascript_function` with a body from
//! `bridge::call_script`, whose arguments are JSON literals.

use std::path::PathBuf;

use gtk::{gio, glib};
use webkit6::prelude::*;
use webkit6::{javascriptcore, soup};

use linux_app::resources;

/// Serve the editor bundle in `dir` under `neoscad-editor:` for every web
/// view of the process (WebKit's default context; registered once, at
/// startup). Without a bundle every request fails and the window says how
/// to build it.
pub fn register_scheme(dir: Option<PathBuf>) {
    let Some(ctx) = webkit6::WebContext::default() else {
        return;
    };
    if let Some(sec) = ctx.security_manager() {
        // Secure, so the page is a secure context (as a WKWebView custom
        // scheme is) and not treated as mixed content.
        sec.register_uri_scheme_as_secure(resources::SCHEME);
    }
    ctx.register_uri_scheme(resources::SCHEME, move |req| {
        let uri = req.uri().map(|u| u.to_string()).unwrap_or_default();
        let found = dir
            .as_deref()
            .and_then(|d| resources::resolve(d, &uri, &resources::nonce()));
        glib::g_debug!(
            "neoscad",
            "editor resource {uri}: {}",
            if found.is_some() {
                "served"
            } else {
                "not found"
            }
        );
        match found {
            Some(r) => {
                let len = r.body.len() as i64;
                let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(r.body));
                let response = webkit6::URISchemeResponse::new(&stream, len);
                response.set_content_type(r.content_type);
                let headers = soup::MessageHeaders::new(soup::MessageHeadersType::Response);
                // The type goes in the headers too: once a response has
                // headers WebKit takes its type from them, and with
                // `nosniff` a script without one is refused.
                headers.append("Content-Type", r.content_type);
                headers.append("Cache-Control", "no-store");
                headers.append("X-Content-Type-Options", "nosniff");
                if let Some(csp) = &r.csp {
                    headers.append("Content-Security-Policy", csp);
                }
                response.set_http_headers(headers);
                req.finish_with_response(&response);
            }
            None => {
                let mut e = glib::Error::new(gio::IOErrorEnum::NotFound, &format!("no {uri}"));
                req.finish_error(&mut e);
            }
        }
    });
}

/// A web view for the editor page, its message handler named `editor`
/// calling `on_message` with each posted message as JSON.
pub fn web_view(on_message: impl Fn(serde_json::Value) + 'static) -> webkit6::WebView {
    let ucm = webkit6::UserContentManager::new();
    ucm.register_script_message_handler_with_reply("editor", None);
    ucm.connect_script_message_with_reply_received(Some("editor"), move |_, value, reply| {
        let json = value
            .to_json(0)
            .and_then(|s| serde_json::from_str(s.as_str()).ok())
            .unwrap_or(serde_json::Value::Null);
        on_message(json);
        // Every post is answered (with null): the page may await it.
        if let Some(ctx) = value.context() {
            reply.return_value(&javascriptcore::Value::new_null(&ctx));
        }
        true
    });
    let web = webkit6::WebView::builder()
        .user_content_manager(&ucm)
        .hexpand(true)
        .vexpand(true)
        .build();
    if let Some(settings) = webkit6::prelude::WebViewExt::settings(&web) {
        // Nothing the page does needs these, and each is a way out of it.
        settings.set_javascript_can_open_windows_automatically(false);
        // NEOSCAD_EDITOR_INSPECT: the web inspector, and the page's
        // console on stdout (a script error that stops the editor loading
        // is otherwise invisible).
        let inspect = std::env::var_os("NEOSCAD_EDITOR_INSPECT").is_some();
        settings.set_enable_developer_extras(inspect);
        settings.set_enable_write_console_messages_to_stdout(inspect);
    }
    // The page never navigates: a link or a script that tried would leave
    // the editor, so only the editor's own scheme loads.
    web.connect_decide_policy(|_, decision, kind| {
        if kind != webkit6::PolicyDecisionType::NavigationAction {
            return false;
        }
        let uri = decision
            .downcast_ref::<webkit6::NavigationPolicyDecision>()
            .and_then(|d| d.navigation_action())
            .and_then(|a| a.request())
            .and_then(|r| r.uri())
            .map(|u| u.to_string())
            .unwrap_or_default();
        if uri.starts_with(&format!("{}:", resources::SCHEME)) {
            false
        } else {
            decision.ignore();
            true
        }
    });
    web.connect_load_changed(|web, event| {
        glib::g_debug!("neoscad", "editor page {event:?} {:?}", web.uri());
    });
    web.connect_load_failed(|_, _, uri, error| {
        glib::g_warning!("neoscad", "the editor page did not load ({uri}): {error}");
        false
    });
    web.load_uri(resources::PAGE_URL);
    web
}

/// Call `NeoSCADEditor.<function>(args…)` and hand its JSON result (or
/// `null`) to `done`.
pub fn call(
    web: &webkit6::WebView,
    function: &str,
    args: &[serde_json::Value],
    done: impl FnOnce(serde_json::Value) + 'static,
) {
    let body = linux_app::bridge::call_script(function, args);
    web.call_async_javascript_function(&body, None, None, None, gio::Cancellable::NONE, move |r| {
        let v = match r {
            Ok(v) => v
                .to_json(0)
                .and_then(|s| serde_json::from_str(s.as_str()).ok())
                .unwrap_or(serde_json::Value::Null),
            Err(e) => {
                glib::g_warning!("neoscad", "editor call failed: {e}");
                serde_json::Value::Null
            }
        };
        done(v);
    });
}
