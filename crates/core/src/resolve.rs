//! Connection resolution — pure (ADR-0001, R1.2, PLAN M0.6).
//!
//! Strict precedence: explicit flags beat the config file's default Profile,
//! which beats the environment, which falls back to `127.0.0.1:6379`.
//!
//! Two rules carry the weight here:
//!
//! 1. **It never prompts.** Where sources conflict the app resolves
//!    deterministically, and the title bar permanently shows the resolved
//!    target *and* its [`Source`]. That readout is the entire mitigation for
//!    resolving silently — it is not optional chrome.
//! 2. **`REDIS_URL` is used wholesale or not at all.** It is never merged at
//!    the component level with `REDIS_HOST`/`REDIS_PORT`/`REDIS_USER`/
//!    `REDIS_PASSWORD`, because a stale export lingering in a shell would then
//!    produce a target nobody can explain.

use crate::config::{Config, Profile};
use crate::state::{Connection, Environment, Source};

/// Where a password comes from — a *description*, not the secret itself.
///
/// Secrets are references (ADR-0002), and resolving a reference means reading
/// an environment variable or running a command, both of which are I/O. So the
/// core says what to fetch and the shell fetches it. A literal passes through
/// only because the config file is refused when it is group- or world-readable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PasswordSource {
    #[default]
    None,
    Literal(String),
    /// The name of an environment variable holding the password.
    Env(String),
    /// A command whose stdout is the password.
    Command(String),
}

/// What is needed to authenticate, as references.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Credentials {
    pub username: Option<String>,
    pub password: PasswordSource,
    pub tls: bool,
}

/// Everything resolution produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub connection: Connection,
    pub credentials: Credentials,
    /// The URL to actually dial, credentials intact.
    ///
    /// Kept apart from [`Connection::target`], which is redacted for display.
    /// Reconstructing one from the other would mean either dialling a redacted
    /// URL or showing a password.
    pub dial_url: String,
}

impl Resolution {
    pub fn dial_url(&self) -> &str {
        &self.dial_url
    }
}

/// What the user asked for on the command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flags {
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub db: Option<u8>,
    pub profile: Option<String>,
    /// `--user`, `--password`, `--tls` — a credential source that always wins,
    /// on top of whatever target and credentials the other flags, a Profile,
    /// or the environment produced. See `resolve()`'s override pass below.
    pub user: Option<String>,
    pub password: Option<String>,
    pub tls: bool,
}

impl Flags {
    fn names_a_target(&self) -> bool {
        self.url.is_some() || self.host.is_some() || self.port.is_some()
    }
}

/// The environment variables resolution looks at. Passed in as data so that
/// resolution stays pure and the precedence matrix is testable as a table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvVars {
    pub redis_url: Option<String>,
    pub redis_host: Option<String>,
    pub redis_port: Option<String>,
    pub redis_user: Option<String>,
    pub redis_password: Option<String>,
}

impl EnvVars {
    fn names_a_target(&self) -> bool {
        self.redis_url.is_some() || self.redis_host.is_some() || self.redis_port.is_some()
    }
}

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 6379;

/// Resolve a Connection from flags, config and environment.
///
/// Never fails and never prompts: the fallback is always `127.0.0.1:6379`.
pub fn resolve(flags: &Flags, config: Option<&Config>, env: &EnvVars) -> Resolution {
    let mut resolution = resolve_target(flags, config, env);
    // `--user`/`--password`/`--tls` always win, on top of whatever the target
    // resolution above produced — a Profile's stored password included. This
    // is the same "explicit flags beat everything" precedence ADR-0001 already
    // states for the target itself, extended to credentials.
    if let Some(user) = &flags.user {
        resolution.credentials.username = Some(user.clone());
    }
    if let Some(password) = &flags.password {
        resolution.credentials.password = PasswordSource::Literal(password.clone());
    }
    if flags.tls {
        resolution.credentials.tls = true;
    }
    resolution
}

fn resolve_target(flags: &Flags, config: Option<&Config>, env: &EnvVars) -> Resolution {
    // 1. An explicit --profile names a Profile, and outranks everything but a
    //    target given directly on the command line.
    if let Some(name) = &flags.profile
        && let Some(profile) = config.and_then(|c| c.profiles.get(name))
    {
        return from_profile(name, profile, flags.db);
    }

    // 2. A target given directly on the command line.
    if flags.names_a_target() {
        let raw = flags.url.clone();
        let target = match &flags.url {
            Some(url) => format_url(url, flags.db),
            None => format_target(
                flags.host.as_deref().unwrap_or(DEFAULT_HOST),
                flags.port.unwrap_or(DEFAULT_PORT),
                flags.db.unwrap_or(0),
            ),
        };
        let environment = infer_environment(&target);
        let dial_url = dial_from(raw.as_deref(), &target);
        return Resolution {
            connection: Connection {
                target,
                environment,
                source: Source::Flag,
            },
            credentials: env_credentials(env),
            dial_url,
        };
    }

    // 3. The config file's default Profile.
    if let Some(config) = config
        && let Some(name) = &config.default_profile
        && let Some(profile) = config.profiles.get(name)
    {
        return from_profile(name, profile, flags.db);
    }

    // 4. The environment. REDIS_URL wholesale, or the discrete set — never both.
    if env.names_a_target() {
        let raw = env.redis_url.clone();
        let target = match &env.redis_url {
            Some(url) => format_url(url, flags.db),
            None => format_target(
                env.redis_host.as_deref().unwrap_or(DEFAULT_HOST),
                env.redis_port
                    .as_deref()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(DEFAULT_PORT),
                flags.db.unwrap_or(0),
            ),
        };
        let environment = infer_environment(&target);
        let dial_url = dial_from(raw.as_deref(), &target);
        return Resolution {
            connection: Connection {
                target,
                environment,
                source: Source::Environment,
            },
            credentials: env_credentials(env),
            dial_url,
        };
    }

    // 5. Nothing was configured.
    let target = format_target(DEFAULT_HOST, DEFAULT_PORT, flags.db.unwrap_or(0));
    let dial_url = dial_from(None, &target);
    Resolution {
        connection: Connection {
            target,
            environment: Environment::Local,
            source: Source::Default,
        },
        credentials: env_credentials(env),
        dial_url,
    }
}

/// The URL to dial: the original when one was given, otherwise assembled from
/// the target. The displayed target is redacted, so it cannot be reused here.
fn dial_from(raw: Option<&str>, target: &str) -> String {
    match raw {
        Some(url) => url.to_string(),
        None if target.contains("://") => target.to_string(),
        None => format!("redis://{target}"),
    }
}

/// `REDIS_USER` and `REDIS_PASSWORD`. Used only when the target itself came
/// from the environment or the command line — never merged into a Profile,
/// whose credentials are stated in the file (ADR-0001).
fn env_credentials(env: &EnvVars) -> Credentials {
    Credentials {
        username: env.redis_user.clone(),
        password: match &env.redis_password {
            Some(p) => PasswordSource::Literal(p.clone()),
            None => PasswordSource::None,
        },
        tls: false,
    }
}

fn from_profile(name: &str, profile: &Profile, db_override: Option<u8>) -> Resolution {
    let db = db_override.or(profile.db).unwrap_or(0);
    let raw = profile.url.clone();
    let target = match &profile.url {
        Some(url) => format_url(url, Some(db)),
        None => format_target(
            profile.host.as_deref().unwrap_or(DEFAULT_HOST),
            profile.port.unwrap_or(DEFAULT_PORT),
            db,
        ),
    };
    // Reference forms are checked before the literal, so a Profile carrying
    // both does not silently use the one ADR-0002 discourages.
    let password = if let Some(var) = &profile.password_env {
        PasswordSource::Env(var.clone())
    } else if let Some(cmd) = &profile.password_command {
        PasswordSource::Command(cmd.clone())
    } else if let Some(literal) = &profile.password {
        PasswordSource::Literal(literal.clone())
    } else {
        PasswordSource::None
    };

    let dial_url = dial_from(raw.as_deref(), &target);
    Resolution {
        connection: Connection {
            target,
            environment: profile.environment(),
            source: Source::Profile(name.to_string()),
        },
        credentials: Credentials {
            username: profile.username.clone(),
            password,
            tls: profile.tls.unwrap_or(false),
        },
        dial_url,
    }
}

fn format_target(host: &str, port: u16, db: u8) -> String {
    format!("{host}:{port}/{db}")
}

fn format_url(url: &str, db: Option<u8>) -> String {
    let url = redact(url);
    match db {
        Some(db) => format!("{}/{db}", url.trim_end_matches('/')),
        None => url,
    }
}

/// Remove any password embedded in a URL.
///
/// The title bar shows the target permanently (ADR-0001), so a password in a
/// `rediss://user:pass@host` URL would sit on screen for the whole session —
/// through every screen-share, screenshot and pasted diagnostic. The userinfo
/// is kept because a username is not a secret and identifies which ACL user is
/// connected, which is exactly the kind of thing the Source readout exists for.
pub fn redact(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let Some((userinfo, host)) = rest.split_once('@') else {
        return url.to_string();
    };
    match userinfo.split_once(':') {
        Some((user, _password)) if !user.is_empty() => format!("{scheme}://{user}:•••@{host}"),
        // `redis://:password@host` — no username, so nothing worth keeping.
        Some(_) => format!("{scheme}://•••@{host}"),
        None => url.to_string(),
    }
}

/// An ad-hoc target that is not loopback or a unix socket is `unknown`, and
/// therefore starts in Read-only Mode (ADR-0004). `unknown` is a real
/// Environment, not a stand-in for "probably fine".
fn infer_environment(target: &str) -> Environment {
    let host = target
        .rsplit('@')
        .next()
        .unwrap_or(target)
        .trim_start_matches("redis://")
        .trim_start_matches("rediss://");
    let is_loopback = host.starts_with("127.0.0.1")
        || host.starts_with("localhost")
        || host.starts_with("[::1]")
        || host.starts_with("::1");
    let is_unix_socket = target.starts_with('/') || target.starts_with("unix:");
    if is_loopback || is_unix_socket {
        Environment::Local
    } else {
        Environment::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_default() -> Config {
        crate::config::parse(
            r#"{
                 "defaultProfile": "staging",
                 "profiles": {
                   "staging": {"host":"cache-01","port":6379,"env":"staging"},
                   "prod":    {"host":"redis.prod","port":6380,"db":2,"env":"prod"}
                 }
               }"#,
        )
        .unwrap()
    }

    fn env_url(url: &str) -> EnvVars {
        EnvVars {
            redis_url: Some(url.into()),
            ..EnvVars::default()
        }
    }

    /// The precedence matrix from ADR-0001, as a table.
    #[test]
    fn precedence_flags_beat_profile_beat_environment_beat_localhost() {
        let cfg = config_with_default();
        let flag_host = Flags {
            host: Some("from-flag".into()),
            ..Flags::default()
        };
        let env = env_url("redis://from-env:6379");

        struct Case<'a> {
            why: &'a str,
            flags: Flags,
            config: Option<&'a Config>,
            env: EnvVars,
            target: &'a str,
            source: Source,
        }

        let cases = vec![
            Case {
                why: "flags outrank everything",
                flags: flag_host.clone(),
                config: Some(&cfg),
                env: env.clone(),
                target: "from-flag:6379/0",
                source: Source::Flag,
            },
            Case {
                why: "default Profile outranks the environment",
                flags: Flags::default(),
                config: Some(&cfg),
                env: env.clone(),
                target: "cache-01:6379/0",
                source: Source::Profile("staging".into()),
            },
            Case {
                why: "environment is used when no config exists",
                flags: Flags::default(),
                config: None,
                env: env.clone(),
                target: "redis://from-env:6379",
                source: Source::Environment,
            },
            Case {
                why: "localhost when nothing at all is configured",
                flags: Flags::default(),
                config: None,
                env: EnvVars::default(),
                target: "127.0.0.1:6379/0",
                source: Source::Default,
            },
            Case {
                why: "--profile selects a non-default Profile",
                flags: Flags {
                    profile: Some("prod".into()),
                    ..Flags::default()
                },
                config: Some(&cfg),
                env: env.clone(),
                target: "redis.prod:6380/2",
                source: Source::Profile("prod".into()),
            },
        ];

        for c in cases {
            let got = resolve(&c.flags, c.config, &c.env).connection;
            assert_eq!(got.target, c.target, "{}", c.why);
            assert_eq!(got.source, c.source, "{}", c.why);
        }
    }

    #[test]
    fn redis_url_is_used_wholesale_and_never_merged_with_the_discrete_variables() {
        // A stale REDIS_HOST export must not contaminate an explicit REDIS_URL.
        let env = EnvVars {
            redis_url: Some("redis://real-target:6379".into()),
            redis_host: Some("stale-leftover".into()),
            redis_port: Some("9999".into()),
            ..EnvVars::default()
        };
        let got = resolve(&Flags::default(), None, &env).connection;
        assert_eq!(got.target, "redis://real-target:6379");
        assert!(!got.target.contains("stale-leftover"));
        assert!(!got.target.contains("9999"));
    }

    #[test]
    fn the_discrete_variables_are_used_when_there_is_no_redis_url() {
        let env = EnvVars {
            redis_host: Some("box".into()),
            redis_port: Some("6390".into()),
            ..EnvVars::default()
        };
        assert_eq!(
            resolve(&Flags::default(), None, &env).connection.target,
            "box:6390/0"
        );
    }

    #[test]
    fn resolution_never_prompts_and_always_produces_a_target() {
        // Every combination resolves to something. There is no "ask the user".
        for config in [None, Some(&config_with_default())] {
            for env in [EnvVars::default(), env_url("redis://x:1")] {
                let got = resolve(&Flags::default(), config, &env).connection;
                assert!(!got.target.is_empty());
            }
        }
    }

    #[test]
    fn an_untagged_remote_target_is_unknown_not_local() {
        let flags = Flags {
            host: Some("10.0.0.9".into()),
            ..Flags::default()
        };
        assert_eq!(
            resolve(&flags, None, &EnvVars::default())
                .connection
                .environment,
            Environment::Unknown
        );
    }

    #[test]
    fn loopback_is_local() {
        for host in ["127.0.0.1", "localhost"] {
            let flags = Flags {
                host: Some(host.into()),
                ..Flags::default()
            };
            assert_eq!(
                resolve(&flags, None, &EnvVars::default())
                    .connection
                    .environment,
                Environment::Local
            );
        }
    }

    #[test]
    fn a_profiles_declared_environment_wins_over_inference() {
        // "cache-01" is not loopback, but the Profile says staging, and a
        // declared Environment is a statement of fact, not a guess.
        let cfg = config_with_default();
        assert_eq!(
            resolve(&Flags::default(), Some(&cfg), &EnvVars::default())
                .connection
                .environment,
            Environment::Staging
        );
    }

    #[test]
    fn an_unknown_profile_name_falls_through_rather_than_failing() {
        let cfg = config_with_default();
        let flags = Flags {
            profile: Some("nonexistent".into()),
            ..Flags::default()
        };
        // Falls through to the default Profile; the name is validated at load.
        assert_eq!(
            resolve(&flags, Some(&cfg), &EnvVars::default())
                .connection
                .source,
            Source::Profile("staging".into())
        );
    }

    #[test]
    fn a_db_flag_overrides_the_profiles_database() {
        let cfg = config_with_default();
        let flags = Flags {
            profile: Some("prod".into()),
            db: Some(7),
            ..Flags::default()
        };
        assert_eq!(
            resolve(&flags, Some(&cfg), &EnvVars::default())
                .connection
                .target,
            "redis.prod:6380/7"
        );
    }
}

#[cfg(test)]
mod credential_tests {
    //! Secrets are references, and the reference has to actually reach the
    //! connection — parsing it and then dropping it is worse than not
    //! supporting it, because the config file looks like it works.

    use super::*;

    fn config(json: &str) -> Config {
        crate::config::parse(json).unwrap()
    }

    fn creds_for(json: &str, profile: &str) -> Credentials {
        let cfg = config(json);
        let flags = Flags {
            profile: Some(profile.into()),
            ..Flags::default()
        };
        resolve(&flags, Some(&cfg), &EnvVars::default()).credentials
    }

    #[test]
    fn password_env_survives_resolution_as_a_reference() {
        let c = creds_for(
            r#"{"profiles":{"p":{"host":"h","passwordEnv":"REDIS_PW"}}}"#,
            "p",
        );
        assert_eq!(c.password, PasswordSource::Env("REDIS_PW".into()));
    }

    #[test]
    fn password_command_survives_resolution() {
        let c = creds_for(
            r#"{"profiles":{"p":{"host":"h","passwordCommand":"pass show redis"}}}"#,
            "p",
        );
        assert_eq!(
            c.password,
            PasswordSource::Command("pass show redis".into())
        );
    }

    #[test]
    fn a_reference_wins_over_a_literal_in_the_same_profile() {
        // Otherwise adding the recommended form to a Profile that already has a
        // literal would silently change nothing.
        let c = creds_for(
            r#"{"profiles":{"p":{"host":"h","password":"hunter2","passwordEnv":"REDIS_PW"}}}"#,
            "p",
        );
        assert_eq!(c.password, PasswordSource::Env("REDIS_PW".into()));
    }

    #[test]
    fn a_username_and_tls_flag_reach_the_connection_too() {
        let c = creds_for(
            r#"{"profiles":{"p":{"host":"h","username":"app","tls":true}}}"#,
            "p",
        );
        assert_eq!(c.username.as_deref(), Some("app"));
        assert!(c.tls);
    }

    #[test]
    fn a_profile_with_no_credentials_asks_for_none() {
        let c = creds_for(r#"{"profiles":{"p":{"host":"h"}}}"#, "p");
        assert_eq!(c.password, PasswordSource::None);
        assert!(c.username.is_none());
    }

    #[test]
    fn the_environment_supplies_credentials_when_the_target_came_from_there() {
        let env = EnvVars {
            redis_url: Some("redis://box:6379".into()),
            redis_user: Some("app".into()),
            redis_password: Some("s3cret".into()),
            ..EnvVars::default()
        };
        let c = resolve(&Flags::default(), None, &env).credentials;
        assert_eq!(c.username.as_deref(), Some("app"));
        assert_eq!(c.password, PasswordSource::Literal("s3cret".into()));
    }

    #[test]
    fn a_profiles_credentials_are_not_merged_with_the_environments() {
        // ADR-0001 forbids component-level merging: a stale REDIS_PASSWORD must
        // not attach itself to a Profile that states its own credentials.
        let cfg = config(r#"{"profiles":{"p":{"host":"h","passwordEnv":"PROFILE_PW"}}}"#);
        let env = EnvVars {
            redis_password: Some("stale".into()),
            redis_user: Some("stale-user".into()),
            ..EnvVars::default()
        };
        let flags = Flags {
            profile: Some("p".into()),
            ..Flags::default()
        };
        let c = resolve(&flags, Some(&cfg), &env).credentials;
        assert_eq!(c.password, PasswordSource::Env("PROFILE_PW".into()));
        assert!(c.username.is_none(), "the stale username must not leak in");
    }

    #[test]
    fn a_password_flag_overrides_a_profiles_stored_password() {
        // `redis-pane --profile prod --password "$(vault read ...)"` has to
        // actually reach the connection, the same way an explicit --host beats
        // a Profile's target (ADR-0001) — the flag is the most explicit thing
        // on the command line, so it wins here too.
        let cfg = config(r#"{"profiles":{"p":{"host":"h","passwordEnv":"PROFILE_PW"}}}"#);
        let flags = Flags {
            profile: Some("p".into()),
            password: Some("override-me".into()),
            ..Flags::default()
        };
        let c = resolve(&flags, Some(&cfg), &EnvVars::default()).credentials;
        assert_eq!(c.password, PasswordSource::Literal("override-me".into()));
    }

    #[test]
    fn a_password_flag_applies_even_when_the_target_came_from_host_and_port() {
        // Before this flag existed, --host/--port had no way to carry a
        // password at all except REDIS_PASSWORD — this is the missing case.
        let flags = Flags {
            host: Some("cache-01".into()),
            password: Some("hunter2".into()),
            ..Flags::default()
        };
        let c = resolve(&flags, None, &EnvVars::default()).credentials;
        assert_eq!(c.password, PasswordSource::Literal("hunter2".into()));
    }

    #[test]
    fn a_user_flag_and_tls_flag_are_applied_the_same_way() {
        let flags = Flags {
            host: Some("cache-01".into()),
            user: Some("app".into()),
            tls: true,
            ..Flags::default()
        };
        let c = resolve(&flags, None, &EnvVars::default()).credentials;
        assert_eq!(c.username.as_deref(), Some("app"));
        assert!(c.tls);
    }

    #[test]
    fn no_flag_credentials_leaves_the_existing_source_untouched() {
        // Regression guard: today's REDIS_PASSWORD-with-host/port behavior
        // must survive unchanged when none of the new flags are set.
        let env = EnvVars {
            redis_password: Some("from-env".into()),
            ..EnvVars::default()
        };
        let flags = Flags {
            host: Some("cache-01".into()),
            ..Flags::default()
        };
        let c = resolve(&flags, None, &env).credentials;
        assert_eq!(c.password, PasswordSource::Literal("from-env".into()));
    }
}

#[cfg(test)]
mod redaction_tests {
    //! A password must never reach the title bar. It is shown permanently
    //! (ADR-0001), so anything in it is on screen for the whole session.

    use super::*;

    #[test]
    fn a_password_in_a_url_is_replaced() {
        assert_eq!(
            redact("rediss://default:s3cret@cache.example.com:6379"),
            "rediss://default:•••@cache.example.com:6379"
        );
    }

    #[test]
    fn the_username_survives_because_it_is_not_a_secret() {
        // Which ACL user is connected is exactly the kind of fact the Source
        // readout exists to make visible.
        assert!(redact("rediss://app-reader:pw@h:6379").starts_with("rediss://app-reader:"));
    }

    #[test]
    fn a_password_with_no_username_leaves_nothing_behind() {
        assert_eq!(redact("redis://:s3cret@h:6379"), "redis://•••@h:6379");
    }

    #[test]
    fn a_url_with_no_credentials_is_untouched() {
        assert_eq!(redact("redis://cache-01:6379"), "redis://cache-01:6379");
        assert_eq!(redact("cache-01:6379/0"), "cache-01:6379/0");
    }

    #[test]
    fn resolution_never_produces_a_target_containing_a_password() {
        let env = EnvVars {
            redis_url: Some("rediss://default:hunter2@h.upstash.io:6379".into()),
            ..EnvVars::default()
        };
        let target = resolve(&Flags::default(), None, &env).connection.target;
        assert!(
            !target.contains("hunter2"),
            "leaked into the title bar: {target}"
        );

        let flags = Flags {
            url: Some("rediss://default:hunter2@h:6379".into()),
            ..Flags::default()
        };
        let target = resolve(&flags, None, &EnvVars::default()).connection.target;
        assert!(
            !target.contains("hunter2"),
            "leaked into the title bar: {target}"
        );
    }
}
