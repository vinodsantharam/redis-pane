//! Resolving a password reference into an actual password (ADR-0002).
//!
//! The core describes where a secret lives; fetching it is I/O and lives here.
//! Nothing in this module logs, and callers must not put the result into an
//! error message — a password in a scrollback is a password on a screen-share.

use redis_pane_core::resolve::PasswordSource;

/// Why a credential could not be obtained.
#[derive(Debug)]
pub enum SecretError {
    /// `passwordEnv` names a variable that is not set.
    MissingEnv(String),
    /// `passwordCommand` failed to start, or exited non-zero.
    ///
    /// Deliberately carries no copy of the command. A `passwordCommand` is by
    /// definition a command that yields a secret, and someone testing their
    /// config may well have written `echo hunter2` — echoing it back would put
    /// the password in the terminal, which is exactly where it must not go.
    CommandFailed { detail: String },
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::MissingEnv(var) => write!(
                f,
                "passwordEnv names {var}, which is not set in this environment"
            ),
            SecretError::CommandFailed { detail } => {
                write!(f, "passwordCommand failed: {detail}")
            }
        }
    }
}

/// Fetch the password a [`PasswordSource`] refers to.
pub fn resolve(source: &PasswordSource) -> Result<Option<String>, SecretError> {
    match source {
        PasswordSource::None => Ok(None),
        PasswordSource::Literal(p) => Ok(Some(p.clone())),
        PasswordSource::Env(var) => std::env::var(var)
            .map(Some)
            .map_err(|_| SecretError::MissingEnv(var.clone())),
        PasswordSource::Command(cmd) => run(cmd).map(Some),
    }
}

/// Run a command and take its stdout as the password.
///
/// Via `sh -c`, because the point of `passwordCommand` is to invoke whatever
/// the user's password manager wants — `pass show redis/prod`, `op read ...`,
/// a pipeline. Only the first line is used and it is trimmed: every credential
/// helper prints a trailing newline, and sending it to `AUTH` would fail in a
/// way that looks like a wrong password.
fn run(command: &str) -> Result<String, SecretError> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|e| SecretError::CommandFailed {
            detail: e.to_string(),
        })?;

    if !output.status.success() {
        // Neither the command nor its stderr is reported: either can carry the
        // secret. The exit status is the most that can be said safely, and the
        // reader has their own config file to hand.
        return Err(SecretError::CommandFailed {
            detail: match output.status.code() {
                Some(code) => format!("exited with status {code}"),
                None => "terminated by a signal".to_string(),
            },
        });
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().next().unwrap_or_default().trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_resolves_to_no_password() {
        assert_eq!(resolve(&PasswordSource::None).unwrap(), None);
    }

    #[test]
    fn a_literal_passes_through() {
        let got = resolve(&PasswordSource::Literal("hunter2".into())).unwrap();
        assert_eq!(got.as_deref(), Some("hunter2"));
    }

    #[test]
    fn an_env_reference_is_read_from_the_environment() {
        // `PATH` rather than a variable this test sets: the workspace forbids
        // unsafe, and `set_var` is unsafe in edition 2024 for good reasons that
        // apply to a multi-threaded test binary.
        let got = resolve(&PasswordSource::Env("PATH".into())).unwrap();
        assert_eq!(got, std::env::var("PATH").ok());
        assert!(got.is_some_and(|v| !v.is_empty()));
    }

    #[test]
    fn a_missing_variable_says_which_one() {
        // The likeliest real failure: a typo, or a shell that never exported it.
        let err = resolve(&PasswordSource::Env("REDIS_PANE_NOT_SET".into())).unwrap_err();
        assert!(err.to_string().contains("REDIS_PANE_NOT_SET"), "{err}");
    }

    #[test]
    fn a_command_supplies_its_first_line_trimmed() {
        // Every credential helper prints a trailing newline; sending it to AUTH
        // would fail in a way that looks exactly like a wrong password.
        let got = resolve(&PasswordSource::Command("printf 'from-cmd\\n'".into())).unwrap();
        assert_eq!(got.as_deref(), Some("from-cmd"));
    }

    #[test]
    fn a_multi_line_command_uses_only_the_first_line() {
        let got = resolve(&PasswordSource::Command(
            "printf 'secret\\nnotes about the secret\\n'".into(),
        ))
        .unwrap();
        assert_eq!(got.as_deref(), Some("secret"));
    }

    #[test]
    fn a_failing_command_reports_its_status_and_nothing_else() {
        // Both the command and its stderr can contain the secret — someone
        // testing their config may literally have written `echo hunter2`.
        let err = resolve(&PasswordSource::Command(
            "echo from-stderr >&2; exit 3".into(),
        ))
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("status 3"), "{msg}");
        assert!(!msg.contains("from-stderr"), "leaked stderr: {msg}");
        assert!(!msg.contains("echo"), "echoed the command back: {msg}");
    }

    #[test]
    fn a_pipeline_works_because_password_managers_need_one() {
        let got = resolve(&PasswordSource::Command(
            "echo ' padded ' | tr -d ' '".into(),
        ))
        .unwrap();
        assert_eq!(got.as_deref(), Some("padded"));
    }
}
