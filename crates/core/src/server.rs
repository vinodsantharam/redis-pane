//! What we know about the server we are talking to (ADR-0007, PLAN M0.8).

/// A parsed `redis_version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

/// The floor: RESP3 and Redis 6.0 (ADR-0007). Below this we refuse to start,
/// naming the version found, rather than admitting the user into a hollowed-out
/// product.
pub const FLOOR: Version = Version {
    major: 6,
    minor: 0,
    patch: 0,
};

impl Version {
    /// Parse the `redis_version` field of `INFO server`.
    ///
    /// Forks append their own suffixes, so anything after the third component
    /// is ignored rather than treated as a parse failure.
    pub fn parse(s: &str) -> Option<Version> {
        let mut parts = s.trim().split(['.', '-', '~']);
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        Some(Version {
            major,
            minor,
            patch,
        })
    }

    pub fn meets_floor(&self) -> bool {
        *self >= FLOOR
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ordinary_versions() {
        assert_eq!(
            Version::parse("8.4.0"),
            Some(Version {
                major: 8,
                minor: 4,
                patch: 0
            })
        );
        assert_eq!(
            Version::parse("6.2.14"),
            Some(Version {
                major: 6,
                minor: 2,
                patch: 14
            })
        );
    }

    #[test]
    fn tolerates_fork_suffixes() {
        // Valkey and distro builds append their own decoration.
        assert_eq!(
            Version::parse("7.2.4-valkey"),
            Some(Version {
                major: 7,
                minor: 2,
                patch: 4
            })
        );
    }

    #[test]
    fn the_floor_is_six_point_zero() {
        assert!(Version::parse("6.0.0").unwrap().meets_floor());
        assert!(Version::parse("8.4.0").unwrap().meets_floor());
        assert!(!Version::parse("5.0.14").unwrap().meets_floor());
        assert!(!Version::parse("4.0.9").unwrap().meets_floor());
    }

    #[test]
    fn nonsense_does_not_parse() {
        assert_eq!(Version::parse("unstable"), None);
        assert_eq!(Version::parse(""), None);
    }
}
