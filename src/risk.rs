use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

/// Declaration order **is** severity order: `Safe < Caution < Danger`.
///
/// The derived `Ord` is what makes the displayed-risk rule a one-liner:
/// `model_risk.max(scanner_risk)`. Never reorder these variants.
#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Safe,
    /// Default on purpose. An unparseable or absent risk resolves upward, never
    /// down — a silent downgrade to `Safe` would strip the warning off a
    /// destructive command.
    #[default]
    Caution,
    Danger,
}

impl Risk {
    /// Models are inconsistent about this field: `"SAFE"`, `"low"`, `"high"`,
    /// `"moderate"`, `"read-only"` all show up in practice. Anything
    /// unrecognized becomes `Caution`.
    pub fn parse_lenient(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "safe" | "low" | "none" | "read-only" | "readonly" | "harmless" => Risk::Safe,
            "danger" | "dangerous" | "high" | "critical" | "destructive" | "severe" => Risk::Danger,
            _ => Risk::Caution,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Risk::Safe => "safe",
            Risk::Caution => "caution",
            Risk::Danger => "danger",
        }
    }

    /// ANSI foreground colour, applied only when stderr is a TTY.
    pub fn ansi(self) -> &'static str {
        match self {
            Risk::Safe => "\x1b[32m",
            Risk::Caution => "\x1b[33m",
            Risk::Danger => "\x1b[1;31m",
        }
    }
}

impl fmt::Display for Risk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl<'de> Deserialize<'de> for Risk {
    /// Deserializes through `Value` rather than `String` so a non-string risk
    /// (some models emit a number) degrades to `Caution` instead of failing the
    /// whole tool-call parse and throwing away an otherwise valid command.
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(de)?;
        Ok(match value {
            serde_json::Value::String(s) => Risk::parse_lenient(&s),
            _ => Risk::Caution,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_order_is_safe_caution_danger() {
        assert!(Risk::Safe < Risk::Caution);
        assert!(Risk::Caution < Risk::Danger);
        assert_eq!(Risk::Safe.max(Risk::Danger), Risk::Danger);
    }

    #[test]
    fn parse_lenient_recognises_synonyms() {
        assert_eq!(Risk::parse_lenient("SAFE"), Risk::Safe);
        assert_eq!(Risk::parse_lenient(" read-only "), Risk::Safe);
        assert_eq!(Risk::parse_lenient("high"), Risk::Danger);
        assert_eq!(Risk::parse_lenient("destructive"), Risk::Danger);
        assert_eq!(Risk::parse_lenient("caution"), Risk::Caution);
    }

    #[test]
    fn unknown_risk_resolves_upward_never_to_safe() {
        assert_eq!(Risk::parse_lenient("moderate"), Risk::Caution);
        assert_eq!(Risk::parse_lenient(""), Risk::Caution);
        assert_eq!(Risk::parse_lenient("banana"), Risk::Caution);
    }

    #[test]
    fn non_string_json_risk_becomes_caution() {
        assert_eq!(serde_json::from_str::<Risk>("2").unwrap(), Risk::Caution);
        assert_eq!(serde_json::from_str::<Risk>("null").unwrap(), Risk::Caution);
        assert_eq!(
            serde_json::from_str::<Risk>("\"danger\"").unwrap(),
            Risk::Danger
        );
    }
}
