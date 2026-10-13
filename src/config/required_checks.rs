use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Mapping-local gate selection, independent of branch protection.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged, try_from = "serde_json::Value")]
pub enum RequiredChecks {
    Names(Vec<String>),
    Rule(CheckRule),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CheckRule {
    Github {},
    All {},
}

impl Default for RequiredChecks {
    fn default() -> Self {
        Self::Rule(CheckRule::Github {})
    }
}

impl RequiredChecks {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        let Self::Names(names) = self else {
            return Ok(());
        };
        let mut seen = HashSet::new();
        if names.is_empty()
            || names.iter().any(|name| {
                name.trim().is_empty()
                    || name.len() > 128
                    || !name.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
                    || !seen.insert(name)
            })
        {
            return Err(
                "required_checks must contain unique non-empty check names of at most 128 ASCII bytes",
            );
        }
        Ok(())
    }
}

impl TryFrom<serde_json::Value> for RequiredChecks {
    type Error = String;

    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        match value {
            serde_json::Value::Array(names) => names
                .into_iter()
                .map(|name| match name {
                    serde_json::Value::String(name) => Ok(name),
                    _ => Err("required_checks names must be strings".into()),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Self::Names),
            serde_json::Value::Object(_) => serde_json::from_value(value)
                .map(Self::Rule)
                .map_err(|_| "invalid required_checks rule".into()),
            _ => Err("required_checks must be a names array or rule object".into()),
        }
    }
}
