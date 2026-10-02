//! The printing recipes ([`super::RECIPES`]) as `docs` entries.
//!
//! An agent reads the recipes in the instructions and then asks `docs`
//! for one by name (`snap_hook`, or just `snap`). Before `docs` knew them
//! it answered "no builtin named 'snap_hook'", and the agent either went
//! without the recipe or spent turns rebuilding it, so `docs` looks the
//! recipes up the way it suggests builtins: exactly, by prefix, then by
//! the diagnostics' "did you mean" distance.

/// One recipe: its module's name and its text (comment and code), as it
/// stands in `recipes.scad`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub name: String,
    pub text: String,
}

/// The recipes in order. Each one is its comment lines and the module
/// that follows them, up to the next comment at the start of a line: a
/// module's body may have its own (indented) comments and definitions,
/// as `thread`'s does.
pub fn all() -> Vec<Recipe> {
    let mut out: Vec<Recipe> = Vec::new();
    let mut text = String::new();
    let mut name: Option<String> = None;
    for line in super::RECIPES.lines() {
        if line.starts_with("//") && name.is_some() {
            out.push(Recipe {
                name: name.take().unwrap_or_default(),
                text: std::mem::take(&mut text),
            });
        }
        if name.is_none()
            && let Some(rest) = line.strip_prefix("module ")
        {
            let n: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            name = Some(n);
        }
        text.push_str(line);
        text.push('\n');
    }
    if let Some(n) = name {
        out.push(Recipe { name: n, text });
    }
    out
}

/// The recipes' names, space-separated, for an index.
pub fn names() -> String {
    all()
        .into_iter()
        .map(|r| r.name)
        .collect::<Vec<_>>()
        .join(" ")
}

/// How a name an agent typed matched a recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    Exact,
    /// A prefix of the name or of one of its `_`-separated words
    /// (`snap`, `hook`, `round`).
    Prefix,
    /// Within the diagnostics' "did you mean" distance (`snaphook`,
    /// `countersunk`).
    Near,
}

/// The shortest prefix that counts: one or two letters match too much
/// to be what the agent meant.
const MIN_PREFIX: usize = 3;

/// The recipes `name` asks for, best kind of match first. Case, spaces
/// and hyphens are forgiven (`Snap hook`, `snap-hook`), since agents
/// write the names they read in prose as well as in code.
pub fn find(name: &str) -> Option<(Match, Vec<Recipe>)> {
    let key: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c == ' ' || c == '-' { '_' } else { c })
        .collect();
    let all = all();
    if let Some(r) = all.iter().find(|r| r.name == key) {
        return Some((Match::Exact, vec![r.clone()]));
    }
    if key.chars().count() >= MIN_PREFIX {
        let by_prefix: Vec<Recipe> = all
            .iter()
            .filter(|r| r.name.starts_with(&key) || r.name.split('_').any(|w| w.starts_with(&key)))
            .cloned()
            .collect();
        if !by_prefix.is_empty() {
            return Some((Match::Prefix, by_prefix));
        }
    }
    let near = session::diag::did_you_mean(&key, all.iter().map(|r| r.name.as_str()))?;
    let r = all.iter().find(|r| r.name == near)?.clone();
    Some((Match::Near, vec![r]))
}

/// The answer to a `docs` name that is a recipe: what it is, then its
/// comment and code to adapt.
pub fn answer(name: &str, how: Match, found: &[Recipe]) -> String {
    let what = if found.len() == 1 {
        format!("the printing recipe {}", found[0].name)
    } else {
        format!(
            "the printing recipes {}",
            found
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let mut text = match how {
        Match::Exact => format!("{} (tested; adapt the numbers):\n", capital(&what)),
        _ => format!("'{name}' is not a builtin; {what} (tested; adapt the numbers):\n"),
    };
    for (i, r) in found.iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        text.push_str(&r.text);
    }
    text
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_recipe_is_found_with_its_comment_and_code() {
        let all = all();
        let names: Vec<&str> = all.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "countersink",
                "rounded_plate",
                "fillet",
                "thread",
                "snap_hook"
            ]
        );
        // Each piece is one recipe, and together they are the whole file.
        for r in &all {
            assert!(r.text.starts_with("// "), "{}", r.text);
            assert!(
                r.text.contains(&format!("module {}(", r.name)),
                "{}",
                r.text
            );
        }
        let joined: String = all.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(joined.trim_end(), super::super::RECIPES.trim_end());
        // thread's indented `function r(u)` stays inside thread.
        let thread = all.iter().find(|r| r.name == "thread").unwrap();
        assert!(thread.text.contains("function r(u)"));
        assert!(!thread.text.contains("snap_hook"));
    }

    #[test]
    fn names_match_exactly_by_prefix_or_near() {
        let name_of = |q: &str| find(q).map(|(m, v)| (m, v[0].name.clone()));
        assert_eq!(
            name_of("snap_hook"),
            Some((Match::Exact, "snap_hook".into()))
        );
        assert_eq!(
            name_of("Snap hook"),
            Some((Match::Exact, "snap_hook".into()))
        );
        assert_eq!(
            name_of("snap-hook"),
            Some((Match::Exact, "snap_hook".into()))
        );
        assert_eq!(name_of("snap"), Some((Match::Prefix, "snap_hook".into())));
        assert_eq!(name_of("hook"), Some((Match::Prefix, "snap_hook".into())));
        assert_eq!(
            name_of("rounded"),
            Some((Match::Prefix, "rounded_plate".into()))
        );
        assert_eq!(name_of("snaphook"), Some((Match::Near, "snap_hook".into())));
        assert_eq!(
            name_of("countersunk"),
            Some((Match::Near, "countersink".into()))
        );
        // Too short to be a prefix, and nothing near.
        assert_eq!(name_of("sn"), None);
        assert_eq!(name_of("gear"), None);
    }

    #[test]
    fn an_answer_says_what_it_is() {
        let (m, v) = find("snap").unwrap();
        let t = answer("snap", m, &v);
        assert!(
            t.starts_with("'snap' is not a builtin; the printing recipe snap_hook"),
            "{t}"
        );
        assert!(t.contains("module snap_hook("), "{t}");
        let (m, v) = find("fillet").unwrap();
        let t = answer("fillet", m, &v);
        assert!(t.starts_with("The printing recipe fillet (tested"), "{t}");
    }
}
