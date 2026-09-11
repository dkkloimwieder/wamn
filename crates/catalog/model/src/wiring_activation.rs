//! Activation changes and their committed history.
//!
//! Activating a wiring is *moving a pointer*, not shipping an artifact
//! (`docs/exe-model.md` R3, "wirings are data"). One statement writes
//! `catalog.wiring_activation`; another appends `catalog.wiring_activation_events`
//! in the same transaction. The driver holds the transaction; this module
//! supplies SQL statements.
//!
//! # Rollback is the same flip
//!
//! There is one write builder here and no `rollback_sql`. [`flip_activation`]
//! takes the confirmed definition hash and the enabled flag as parameters, so
//! the same statement performs all three operational moves:
//!
//! * activate — flip onto the newly gated hash with `enabled = true`;
//! * roll back — flip onto the *prior* hash ([`previous_confirmed_definition`])
//!   with `enabled = true`;
//! * take a wiring dark — the same key with `enabled = false`, which
//!   `catalog.validate_wiring_activation()` returns early for and therefore can
//!   never refuse.
//!
//! Because the pointer's primary key is `(tenant, package, environment,
//! wiring)`, every one of those is an `UPDATE` of one row after the first
//! activation. Rollback is not a compensating action with its own failure modes;
//! it is the forward path with an older argument.
//!
//! # Where the tenant comes from
//!
//! Every statement scopes to `app.tenant` rather than accepting a tenant
//! parameter. The management principal is a superuser or the schema owner, and a
//! superuser bypasses row-level security outright — so `FORCE ROW LEVEL
//! SECURITY` cannot be what keeps a flip inside its tenant. Reading the claim
//! makes the wrong tenant unrepresentable instead of merely rejected, and an
//! unset claim fails closed on the `NOT NULL` column.
//!
//! # What the pure tests cannot cover (SR12)
//!
//! These are statements, not behaviour: nothing here observes the activation
//! trigger refusing a stale definition or activation history surviving a
//! committed transaction. Those
//! live in `crates/catalog/model/tests/wiring_activation_live.rs` against a
//! throwaway PostgreSQL.

/// The flip: activate, roll back, or take dark — one statement for all three.
///
/// Params: package id, environment, wiring id, confirmed definition hash,
/// enabled. `catalog.validate_wiring_activation()` refuses an enabling flip onto
/// a definition whose exact package version is not a member of this
/// environment's effective release, tombstoned, or absent.
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
    use super::{flip_activation, previous_confirmed_definition, record_activation_event};

    fn statements() -> [&'static str; 3] {
        [
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
