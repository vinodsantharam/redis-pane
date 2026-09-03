//! Config schema and validation — pure (ADR-0002, ADR-0003, PLAN M0.7).
//!
//! Reading the file is a shell concern; describing and validating it is not.
//!
//! **Unknown fields are a parse error, never ignored.** The app only ever reads
//! this file, so every byte in it was typed by hand, which makes a misspelling
//! the likeliest defect — and a misspelled `passwordEnv` is a credential
//! silently dropped. A Profile with no `env` gets `Unknown`, never `Local`
//! (ADR-0004).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::state::Environment;

/// The whole config file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// Which Profile is used when no `--profile` is given.
    pub default_profile: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

/// A named target the user has written down. A Profile is not a Connection: it
/// is the description, not the live thing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Profile {
    /// Used wholesale when present; never merged with `host`/`port`.
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub db: Option<u8>,
    pub username: Option<String>,
    /// A literal password. Honoured, but the file is refused when it is group-
    /// or world-readable, and such Profiles are badged in the UI.
    pub password: Option<String>,
    /// Names an environment variable holding the password. Preferred.
    pub password_env: Option<String>,
    /// A command run at connect time whose stdout is the password. Preferred.
    pub password_command: Option<String>,
    pub tls: Option<bool>,
    /// JSON has no comments, so this stands in for one — and is rendered next
    /// to the Connection, which is better than a comment for the purpose
    /// people would use one (ADR-0002).
    pub note: Option<String>,
    /// Absent means [`Environment::Unknown`], which starts in Read-only Mode.
    pub env: Option<Environment>,
}

impl Profile {
    /// The Environment this Profile declares. Untagged means `unknown`, which
    /// is a real Environment and not a synonym for `local` (ADR-0004).
    pub fn environment(&self) -> Environment {
        self.env.unwrap_or(Environment::Unknown)
    }

    /// Whether this Profile carries a literal password rather than a reference.
    pub fn has_literal_password(&self) -> bool {
        self.password.is_some()
    }
}

/// Why a config file was rejected. Every variant names the file so the message
/// can be acted on without guessing (R1.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// Malformed JSON, or a field the app does not recognise.
    Parse {
        line: usize,
        column: usize,
        detail: String,
    },
    /// `defaultProfile` names a Profile that is not in the file.
    UnknownDefaultProfile(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Parse {
                line,
                column,
                detail,
            } => {
                write!(f, "line {line}, column {column}: {detail}")
            }
            ConfigError::UnknownDefaultProfile(name) => {
                write!(f, "defaultProfile names \"{name}\", which is not defined")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Parse and validate a config file's contents.
///
/// Pure: takes the text, returns a `Config` or a located error. Reading the
/// file, and refusing it for being too readable, belong to the shell.
pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let config: Config = serde_json::from_str(text).map_err(|e| ConfigError::Parse {
        line: e.line(),
        column: e.column(),
        // serde_json appends its own " at line N column M"; we print the
        // location ourselves, so saying it twice just makes the message harder
        // to read at the moment someone is trying to fix a typo.
        detail: strip_location(&e.to_string()),
    })?;

    if let Some(name) = &config.default_profile
        && !config.profiles.contains_key(name)
    {
        return Err(ConfigError::UnknownDefaultProfile(name.clone()));
    }
    Ok(config)
}

fn strip_location(message: &str) -> String {
    match message.rfind(" at line ") {
        Some(i) => message[..i].to_string(),
        None => message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_file_parses() {
        let c =
            parse(r#"{"profiles":{"staging":{"host":"cache-01","port":6379,"env":"staging"}}}"#)
                .unwrap();
        let p = &c.profiles["staging"];
        assert_eq!(p.host.as_deref(), Some("cache-01"));
        assert_eq!(p.environment(), Environment::Staging);
    }

    #[test]
    fn an_empty_object_is_valid() {
        assert_eq!(parse("{}").unwrap(), Config::default());
    }

    #[test]
    fn a_profile_without_env_is_unknown_never_local() {
        let c = parse(r#"{"profiles":{"box":{"host":"10.0.0.9"}}}"#).unwrap();
        assert_eq!(c.profiles["box"].environment(), Environment::Unknown);
    }

    #[test]
    fn a_misspelled_password_env_is_refused_not_ignored() {
        // The whole point of deny_unknown_fields: this typo would otherwise
        // drop the credential silently.
        let err = parse(r#"{"profiles":{"p":{"passwordEnvironment":"REDIS_PW"}}}"#).unwrap_err();
        match err {
            ConfigError::Parse { detail, .. } => {
                assert!(detail.contains("passwordEnvironment"), "got: {detail}");
            }
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn a_parse_error_carries_a_line_and_column() {
        let err = parse("{\n  \"profiles\": {\n    oops\n  }\n}").unwrap_err();
        match err {
            ConfigError::Parse { line, column, .. } => {
                assert_eq!(line, 3);
                assert!(column > 0);
            }
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn the_location_is_stated_once_not_twice() {
        let err = parse(r#"{"profiles":{"p":{"nope":1}}}"#).unwrap_err();
        let msg = err.to_string();
        assert_eq!(msg.matches("line ").count(), 1, "got: {msg}");
    }

    #[test]
    fn default_profile_must_exist() {
        let err = parse(r#"{"defaultProfile":"nope","profiles":{"p":{}}}"#).unwrap_err();
        assert_eq!(err, ConfigError::UnknownDefaultProfile("nope".into()));
    }

    #[test]
    fn every_environment_name_round_trips() {
        for (text, expected) in [
            ("local", Environment::Local),
            ("staging", Environment::Staging),
            ("prod", Environment::Prod),
            ("unknown", Environment::Unknown),
        ] {
            let c = parse(&format!(r#"{{"profiles":{{"p":{{"env":"{text}"}}}}}}"#)).unwrap();
            assert_eq!(c.profiles["p"].environment(), expected);
        }
    }

    #[test]
    fn an_unrecognised_environment_is_refused() {
        assert!(parse(r#"{"profiles":{"p":{"env":"production"}}}"#).is_err());
    }

    #[test]
    fn a_literal_password_is_detected_so_the_ui_can_badge_it() {
        let c = parse(r#"{"profiles":{"p":{"password":"hunter2"}}}"#).unwrap();
        assert!(c.profiles["p"].has_literal_password());
    }
}
