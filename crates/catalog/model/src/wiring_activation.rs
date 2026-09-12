//! Wiring activation decisions, statements, and committed history.
//!
//! The driver reads [`activation_facts`] and calls [`validate_wiring_activation`].
//! It writes the activation and its history in that same transaction.
//! An enabled activation requires an exact definition in the environment release.
//! A tombstone, which records a retired wiring, prevents activation.
//! Disabling an activation does not require either condition.
//!
//! Every statement reads the tenant from `app.tenant`.
//! PostgreSQL retains its row permissions and integrity constraints.

use std::fmt;

/// Stored facts for one requested wiring definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WiringActivationFacts {
    pub tombstoned: bool,
    pub definition_in_release: bool,
}

/// The reason that an enabled activation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WiringActivationErrorKind {
    Tombstoned,
    DefinitionNotInRelease,
}

/// A refused activation and its package, environment, and wiring.
#[derive(Debug)]
pub struct WiringActivationError {
    kind: WiringActivationErrorKind,
    package_id: String,
    environment: String,
    wiring_id: String,
}

impl WiringActivationError {
    /// Return the refusal reason without parsing its text.
    pub fn kind(&self) -> WiringActivationErrorKind {
        self.kind
    }
}

impl fmt::Display for WiringActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let refusal = match self.kind {
            WiringActivationErrorKind::Tombstoned => "wiring-activation-tombstoned",
            WiringActivationErrorKind::DefinitionNotInRelease => {
                "wiring-activation-definition-not-in-effective-release"
            }
        };
        write!(
            formatter,
            "{refusal}: {}/{}/{}",
            self.package_id, self.environment, self.wiring_id
        )
    }
}

impl std::error::Error for WiringActivationError {}

/// Refuse an enabled activation when its stored facts do not permit it.
pub fn validate_wiring_activation(
    package_id: &str,
    environment: &str,
    wiring_id: &str,
    enabled: bool,
    facts: WiringActivationFacts,
) -> Result<(), WiringActivationError> {
    let kind = if !enabled {
        return Ok(());
    } else if facts.tombstoned {
        WiringActivationErrorKind::Tombstoned
    } else if !facts.definition_in_release {
        WiringActivationErrorKind::DefinitionNotInRelease
    } else {
        return Ok(());
    };
    Err(WiringActivationError {
        kind,
        package_id: package_id.to_owned(),
        environment: environment.to_owned(),
        wiring_id: wiring_id.to_owned(),
    })
}

/// Read retirement and release membership for one exact definition.
///
/// Parameters: package ID, environment, wiring ID, confirmed definition hash.
/// The driver reads these facts in the transaction that writes the activation.
pub fn activation_facts() -> &'static str {
    "SELECT EXISTS (
        SELECT 1 FROM catalog.wiring_tombstones AS dead
         WHERE dead.tenant_id = NULLIF(current_setting('app.tenant', true), '')
           AND dead.package_id = $1 AND dead.environment = $2 AND dead.wiring_id = $3
    ) AS tombstoned, EXISTS (
        SELECT 1 FROM catalog.wirings AS wiring
          JOIN catalog.effective_release_heads AS head
            ON head.tenant_id = wiring.tenant_id AND head.environment = $2
          JOIN catalog.effective_release_packages AS member
            ON member.tenant_id = head.tenant_id
           AND member.effective_release_id = head.effective_release_id
           AND member.package_id = wiring.package_id
           AND member.package_version = wiring.package_version
         WHERE wiring.tenant_id = NULLIF(current_setting('app.tenant', true), '')
           AND wiring.package_id = $1 AND wiring.wiring_id = $3 AND wiring.wiring_hash = $4
    ) AS definition_in_release"
}

/// Write an activation, rollback, or disabled state.
///
/// Parameters: package ID, environment, wiring ID, confirmed definition hash, enabled.
/// The driver first calls [`validate_wiring_activation`] in the same transaction.
pub fn flip_activation() -> &'static str {
    "\
INSERT INTO catalog.wiring_activation \
       (tenant_id, package_id, environment, wiring_id, \
        confirmed_definition_hash, enabled, changed_at) \
VALUES (NULLIF(current_setting('app.tenant', true), ''), $1, $2, $3, $4, $5, now()) \
ON CONFLICT (tenant_id, package_id, environment, wiring_id) DO UPDATE \
   SET confirmed_definition_hash = EXCLUDED.confirmed_definition_hash, \
       enabled = EXCLUDED.enabled, \
       changed_at = EXCLUDED.changed_at"
}

/// Append the provenance row for one flip, in the flip's own transaction.
///
/// Params: package id, environment, wiring id, enabled, confirmed definition
/// hash, source environment, changed by, reason. `source_environment` is the
/// promote half, so a local flip binds `NULL` to it.
///
/// NO report id is passed, from either side (wamn-0h0g.8.5.6). The gate report
/// keys on the wiring hash, promotion copies a document byte-for-byte, and
/// `confirmed_definition_hash` is already that hash — so a `source_gate_report_id`
/// parameter would write the row's own confirmed hash into a second column of
/// the same row.
pub fn record_activation_event() -> &'static str {
    "\
INSERT INTO catalog.wiring_activation_events \
       (tenant_id, package_id, environment, wiring_id, enabled, \
        confirmed_definition_hash, source_environment, \
        changed_by, reason) \
VALUES (NULLIF(current_setting('app.tenant', true), ''), \
        $1, $2, $3, $4, $5, $6, $7, $8) \
RETURNING event_seq"
}

/// The hash a rollback flips back to: the last one this pointer served that is
/// not the one it serves now.
///
/// Params: package id, environment, wiring id, currently confirmed hash. Reads
/// the append-only provenance rather than a "previous" column, because there is
/// no such column — the pointer holds one hash and the log holds the history.
/// Returns no row when the wiring has never served anything else, which is the
/// honest answer: there is nothing to roll back to.
pub fn previous_confirmed_definition() -> &'static str {
    "\
SELECT confirmed_definition_hash \
  FROM catalog.wiring_activation_events \
 WHERE tenant_id = NULLIF(current_setting('app.tenant', true), '') \
   AND package_id = $1 AND environment = $2 AND wiring_id = $3 \
   AND enabled AND confirmed_definition_hash <> $4 \
 ORDER BY event_seq DESC \
 LIMIT 1"
}

#[cfg(test)]
mod tests {
    use super::{
        WiringActivationErrorKind, WiringActivationFacts, activation_facts, flip_activation,
        previous_confirmed_definition, record_activation_event, validate_wiring_activation,
    };

    #[test]
    fn disabled_activation_accepts_each_retirement_and_membership_state() {
        for tombstoned in [false, true] {
            for definition_in_release in [false, true] {
                validate_wiring_activation(
                    "shop",
                    "prod",
                    "orders-create",
                    false,
                    WiringActivationFacts {
                        tombstoned,
                        definition_in_release,
                    },
                )
                .expect("disabling does not require a current definition");
            }
        }
    }

    #[test]
    fn retirement_takes_precedence_over_missing_release_membership() {
        let error = validate_wiring_activation(
            "shop",
            "prod",
            "orders-create",
            true,
            WiringActivationFacts {
                tombstoned: true,
                definition_in_release: false,
            },
        )
        .expect_err("a retired wiring cannot be enabled");
        assert_eq!(error.kind(), WiringActivationErrorKind::Tombstoned);
        assert_eq!(
            error.to_string(),
            "wiring-activation-tombstoned: shop/prod/orders-create"
        );
    }

    fn statements() -> [&'static str; 4] {
        [
            activation_facts(),
            flip_activation(),
            record_activation_event(),
            previous_confirmed_definition(),
        ]
    }

    /// One write builder names the pointer, and it sets both of the columns a
    /// flip can move — which is what makes rollback the forward path rather than
    /// a second mechanism with its own bugs.
    #[test]
    fn the_only_pointer_write_carries_both_the_hash_and_the_enabled_flag() {
        let writers: Vec<&str> = statements()
            .into_iter()
            .filter(|sql| {
                sql.contains("INTO catalog.wiring_activation ")
                    || sql.contains("UPDATE catalog.wiring_activation")
                    || sql.contains("DELETE FROM catalog.wiring_activation")
            })
            .collect();
        assert_eq!(
            writers.as_slice(),
            [flip_activation()],
            "a second pointer writer is a second activation verb"
        );

        let flip = flip_activation();
        for assignment in [
            "confirmed_definition_hash = EXCLUDED.confirmed_definition_hash",
            "enabled = EXCLUDED.enabled",
        ] {
            assert!(
                flip.contains(assignment),
                "the flip must move {assignment:?}; a flip that cannot is not a rollback"
            );
        }
        assert!(
            flip.contains("ON CONFLICT (tenant_id, package_id, environment, wiring_id) DO UPDATE"),
            "the flip must land on the pointer's own key, so the first \
             activation and every rollback are the same statement"
        );
    }

    /// A statement that took a tenant parameter would let a superuser driver —
    /// which bypasses RLS — write into another tenant's pointer by passing the
    /// wrong string.
    #[test]
    fn no_statement_lets_the_caller_choose_the_tenant() {
        for sql in statements() {
            assert!(
                sql.contains("NULLIF(current_setting('app.tenant', true), '')"),
                "statement does not scope to the app.tenant claim: {sql}"
            );
        }
    }
}
