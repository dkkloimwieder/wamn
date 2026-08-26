//! The closed authority a trusted caller selects its credential under.
//!
//! `wamn-0h0g.22.14` ruled this vocabulary closed and total: one role family per
//! class, no wildcard arm, no default arm, and an added or unmapped class is a
//! COMPILE ERROR rather than a runtime fallback. The projection onto the
//! provisioning families lives beside `WorkloadRoleFamily` in
//! `wamn-control-provision`, because that is where the target type is defined.
//!
//! # Why this lives here and not with the role families
//!
//! `wamn-control-provision` is a DEV-ONLY dependency of `wamn-runtime` — the
//! shipped runtime deliberately links no provisioner. The class is needed by
//! production runtime code (the credential provider and both pool caches), so
//! it lives in the crate both sides already depend on in their normal graph.
//!
//! # Why there is deliberately no parser
//!
//! A class originates ONLY in trusted host composition or operator-owned
//! workload binding (`wamn-0h0g.22.8`), so a new class is a code change and
//! never a configuration string. Adding `FromStr`, `From<&str>` or a
//! `Deserialize` impl here would reopen precisely the guest-selected-class path
//! the ruling closes. [`AuthorityClass::as_str`] is one-way on purpose.

use std::fmt;

/// Which authority a trusted caller acts under when it selects a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthorityClass {
    /// Tenant component `wamn:postgres` imports.
    GuestSql,
    /// Executor run-state and catalog work, and the wiring doorbell listener.
    ExecutorPlatform,
    /// Connection-HTTP authorization and effect-snapshot operations.
    CallableHttp,
    /// Materializer-owned event consumption and admission SQL.
    EventMaterializer,
}

impl AuthorityClass {
    /// Every class, in declaration order, for table-driven tests.
    pub const ALL: [Self; 4] = [
        Self::GuestSql,
        Self::ExecutorPlatform,
        Self::CallableHttp,
        Self::EventMaterializer,
    ];

    /// Stable label for cache keys, telemetry and error text.
    ///
    /// One-way by design: no inverse exists, so a label can never be turned
    /// back into a class. See the module doc.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GuestSql => "guest-sql",
            Self::ExecutorPlatform => "executor-platform",
            Self::CallableHttp => "callable-http",
            Self::EventMaterializer => "event-materializer",
        }
    }
}

impl fmt::Display for AuthorityClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::AuthorityClass;

    /// `ALL` is a convenience, so it gets its own guard: the compile-error
    /// guarantee for a new variant comes from the exhaustive matches in
    /// `as_str` and in the provisioning projection, not from this array.
    #[test]
    fn all_carries_four_distinct_classes() {
        let mut seen = Vec::new();
        for class in AuthorityClass::ALL {
            assert!(
                !seen.contains(&class),
                "AuthorityClass::ALL lists {class} more than once"
            );
            seen.push(class);
        }
        assert_eq!(seen.len(), 4, "AuthorityClass::ALL must list every class");
    }

    #[test]
    fn labels_are_distinct_and_stable() {
        let mut labels: Vec<&str> = AuthorityClass::ALL.iter().map(|c| c.as_str()).collect();
        labels.sort_unstable();
        assert_eq!(
            labels,
            [
                "callable-http",
                "event-materializer",
                "executor-platform",
                "guest-sql"
            ],
            "authority-class labels are cache-key and telemetry surface; changing one is a \
             deliberate break, not a rename"
        );
    }

    #[test]
    fn display_matches_the_label() {
        for class in AuthorityClass::ALL {
            assert_eq!(class.to_string(), class.as_str());
        }
    }
}
