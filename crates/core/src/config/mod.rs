//! Config schema and validation — pure (ADR-0002, ADR-0003).
//!
//! Reading the file is a shell concern; describing and validating it is not.
//! Unknown fields are a parse error, never ignored: the file is hand-authored,
//! so a misspelled `passwordEnv` is a credential silently dropped. A Profile
//! with no `env` gets `unknown`, never `local` (ADR-0004).
