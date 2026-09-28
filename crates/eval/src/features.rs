//! OpenSCAD's experimental features (`src/Feature.cc`), switched on with
//! `--enable`.
//!
//! Every host (the command line, `serve`, MCP, the app through `ffi`)
//! parses `--enable` names into one [`Features`] set, which reaches the
//! evaluator as [`crate::Options::features`] and any other stage that
//! needs it (an exporter's `predictible-output`) from the same set, so a
//! name means the same thing everywhere. Off, each feature behaves exactly
//! as the nightly does without the flag: its builtins warn "Experimental
//! builtin function '...' is not enabled" and return `undef`.

/// One experimental feature, in `Feature.cc`'s order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    Roof,
    InputDriverDbus,
    LazyUnion,
    VertexObjectRenderersIndexing,
    /// `textmetrics()`, `fontmetrics()` and `is_object()`.
    TextMetrics,
    /// `import()` as a function reading JSON.
    ImportFunction,
    /// `object()` and `has_key()`.
    ObjectFunction,
    PredictibleOutput,
    /// `v.xy`-style member access on lists.
    VectorSwizzle,
    DiscretizationByError,
    AiFeatures,
    UnicodeIdentifiers,
}

impl Feature {
    /// Every feature, in `Feature.cc`'s order (what `--enable all` turns
    /// on, and the order `--help` lists them in).
    pub const ALL: [Feature; 12] = [
        Feature::Roof,
        Feature::InputDriverDbus,
        Feature::LazyUnion,
        Feature::VertexObjectRenderersIndexing,
        Feature::TextMetrics,
        Feature::ImportFunction,
        Feature::ObjectFunction,
        Feature::PredictibleOutput,
        Feature::VectorSwizzle,
        Feature::DiscretizationByError,
        Feature::AiFeatures,
        Feature::UnicodeIdentifiers,
    ];

    /// The `--enable` name.
    pub fn name(self) -> &'static str {
        match self {
            Feature::Roof => "roof",
            Feature::InputDriverDbus => "input-driver-dbus",
            Feature::LazyUnion => "lazy-union",
            Feature::VertexObjectRenderersIndexing => "vertex-object-renderers-indexing",
            Feature::TextMetrics => "textmetrics",
            Feature::ImportFunction => "import-function",
            Feature::ObjectFunction => "object-function",
            Feature::PredictibleOutput => "predictible-output",
            Feature::VectorSwizzle => "vector-swizzle",
            Feature::DiscretizationByError => "discretization-by-error",
            Feature::AiFeatures => "ai-features",
            Feature::UnicodeIdentifiers => "unicode-identifiers",
        }
    }

    pub fn from_name(name: &str) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.name() == name)
    }

    /// Whether neoscad implements it. The others are accepted and warned
    /// about ("not supported by neoscad"), so a program run with
    /// OpenSCAD's flags is told what it is missing rather than silently
    /// evaluating differently.
    pub fn supported(self) -> bool {
        matches!(
            self,
            Feature::TextMetrics
                | Feature::ImportFunction
                | Feature::ObjectFunction
                | Feature::PredictibleOutput
                | Feature::VectorSwizzle
        )
    }

    fn bit(self) -> u32 {
        1 << self as u32
    }
}

/// A set of [`Feature`]s: empty by default, as in OpenSCAD.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Features(u32);

impl Features {
    pub const NONE: Features = Features(0);

    pub fn has(self, f: Feature) -> bool {
        self.0 & f.bit() != 0
    }

    pub fn with(self, f: Feature) -> Features {
        Features(self.0 | f.bit())
    }

    pub fn insert(&mut self, f: Feature) {
        self.0 |= f.bit();
    }

    pub fn union(self, o: Features) -> Features {
        Features(self.0 | o.0)
    }

    /// Whether any feature that makes object values is on: only then can
    /// a program hold one.
    pub fn objects(self) -> bool {
        self.has(Feature::TextMetrics)
            || self.has(Feature::ImportFunction)
            || self.has(Feature::ObjectFunction)
    }

    /// The set as a number, for fingerprints.
    pub fn bits(self) -> u32 {
        self.0
    }

    /// The features `--enable` names turn on, as OpenSCAD reads them:
    /// `all` turns on every feature and ends the list, and unknown names
    /// (and neoscad's own `part`) are skipped. Unsupported features are
    /// included; they change nothing.
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> Features {
        let mut out = Features::NONE;
        for n in names {
            let n = n.as_ref();
            if n == "all" {
                for f in Feature::ALL {
                    out.insert(f);
                }
                break;
            }
            if let Some(f) = Feature::from_name(n) {
                out.insert(f);
            }
        }
        out
    }

    /// The features in the set, in `Feature.cc`'s order.
    pub fn iter(self) -> impl Iterator<Item = Feature> {
        Feature::ALL.into_iter().filter(move |f| self.has(*f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_all_ends_the_list() {
        for f in Feature::ALL {
            assert_eq!(Feature::from_name(f.name()), Some(f));
        }
        let s = Features::from_names(&["textmetrics", "nope", "part"]);
        assert!(s.has(Feature::TextMetrics));
        assert_eq!(s.iter().count(), 1);
        assert!(s.objects());
        let all = Features::from_names(&["all"]);
        assert_eq!(all.iter().count(), Feature::ALL.len());
        assert!(!Features::NONE.objects());
        assert!(!Features::NONE.with(Feature::VectorSwizzle).objects());
    }
}
