//! A result's submission: an issue on the benchmarks repository made from
//! its issue form, which the repository's validation Action reads.
//!
//! The contract with that repository (docs/community-bench.md,
//! "Submission format"): the form is [`TEMPLATE`]; its field [`FIELD_RESULT`]
//! (label [`LABEL_RESULT`], `render: json`) holds the result JSON, and the
//! optional [`FIELD_NOTES`] (label [`LABEL_NOTES`]) free text. An issue
//! made through the form has a body of `### <label>` sections, the JSON
//! fenced as ```json; [`issue_body`] writes exactly that, so an issue made
//! with `gh issue create` parses the same as one made in the browser.

/// The repository results are submitted to.
pub const BENCH_REPO: &str = "neoscad/benchmarks";
/// The issue form's file in `.github/ISSUE_TEMPLATE/`.
pub const TEMPLATE: &str = "submit.yml";
/// The form's field ids (for pre-filling through the URL) and labels (the
/// body's section headings).
pub const FIELD_RESULT: &str = "result";
pub const LABEL_RESULT: &str = "Result JSON";
pub const FIELD_NOTES: &str = "notes";
pub const LABEL_NOTES: &str = "Notes";
/// The label the form applies. A `gh issue create` submission carries
/// none: someone without triage rights on the repository cannot label an
/// issue, and asking would fail the whole submission. So the validation
/// recognises a submission by its body, not its label.
pub const ISSUE_LABEL: &str = "benchmark";

/// Longer pre-filled URLs are cut off by browsers or refused by GitHub
/// (its limit is about 8 KB); past this the result is pasted by hand.
pub const MAX_URL: usize = 8000;

/// The body of the issue the form would make for `json`.
pub fn issue_body(json: &str, notes: Option<&str>) -> String {
    let notes = notes
        .filter(|n| !n.trim().is_empty())
        .unwrap_or("_No response_");
    format!(
        "### {LABEL_RESULT}\n\n```json\n{}\n```\n\n### {LABEL_NOTES}\n\n{notes}\n",
        json.trim_end()
    )
}

/// The new-issue URL with the form, the title and (when it fits) the
/// result pre-filled, and whether the result is in it: past [`MAX_URL`]
/// the URL leaves it out, so the caller can tell the user to paste it.
pub fn issue_url(title: &str, json: &str) -> (String, bool) {
    let base = format!(
        "https://github.com/{BENCH_REPO}/issues/new?template={TEMPLATE}&title={}",
        percent_encode(title)
    );
    let full = format!("{base}&{FIELD_RESULT}={}", percent_encode(json));
    if full.len() <= MAX_URL {
        (full, true)
    } else {
        (base, false)
    }
}

/// RFC 3986 percent-encoding of everything but the unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_has_the_form_sections() {
        let b = issue_body("{\"schema\":1}\n", None);
        assert_eq!(
            b,
            "### Result JSON\n\n```json\n{\"schema\":1}\n```\n\n### Notes\n\n_No response_\n"
        );
    }

    #[test]
    fn url_prefills_or_falls_back() {
        let (u, full) = issue_url("neoscad 0.1.1 on x", "{\"a\": 1}");
        assert!(full);
        assert_eq!(
            u,
            "https://github.com/neoscad/benchmarks/issues/new?template=submit.yml\
             &title=neoscad%200.1.1%20on%20x&result=%7B%22a%22%3A%201%7D"
        );
        let big = "x".repeat(MAX_URL);
        let (u, full) = issue_url("t", &big);
        assert!(!full);
        assert!(!u.contains("result="));
    }
}
