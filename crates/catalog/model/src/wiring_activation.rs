//! The activation verb — one flip, one provenance row, one doorbell.
//!
//! Activating a wiring is *moving a pointer*, not shipping an artifact
//! (`docs/exe-model.md` R3, "wirings are data"). One statement writes
//! `catalog.wiring_activation`, one appends `catalog.wiring_activation_events`,
//! and the DDL's `wiring_activation_doorbell` trigger rings serving processes
//! from inside that same transaction. The driver holds the transaction; this
//! module is text, exactly as `wamn_control_provision::publish_release` is text.
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
//! trigger refusing a stale definition, the doorbell firing on commit, or the
//! `enabled` predicate hiding a rolled-back pointer from the read path. Those
//! live in `crates/catalog/model/tests/wiring_activation_live.rs` against a
//! throwaway PostgreSQL.

use serde::{Deserialize, Serialize};

/// The `LISTEN` channel the activation doorbell rings.
///
/// Named like the run plane's `wamn_run_outcome` because it is the same
/// mechanism: a payload delivered on commit to whoever is listening, never
/// queued for whoever is not.
pub const WIRING_ACTIVATION_CHANNEL: &str = "wamn_wiring_activation";

/// One doorbell payload — what `catalog.notify_wiring_activation()` builds.
///
/// This is the wire shape of the notification, so it is the frozen contract
/// between the DDL and every listener. A subscriber parses the payload into this
/// and hands it to its resolution cache; the cache's entry point is
/// wamn-0h0g.16.5's, not this crate's.
///
/// `confirmed_definition_hash` rather than a version integer is deliberate: the
/// pointer stores the hash, so a join-free trigger can only carry the hash — and
/// the hash is the finer identity anyway, since it is the byte identity of the
/// exact document the version names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WiringActivationNotice {
    /// Tenant whose pointer moved.
    pub tenant_id: String,
    /// Package the wiring belongs to.
    pub package_id: String,
    /// Environment whose pointer moved — activation is environment-scoped.
    pub environment: String,
    /// The wiring whose enabled definition changed.
    pub wiring_id: String,
    /// Whether the wiring serves after this flip. `false` is a wiring taken
    /// dark, and is as much a cache invalidation as `true`.
    pub enabled: bool,
    /// The definition hash the pointer now confirms.
    pub confirmed_definition_hash: String,
}

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

/// The env-hot read: resolve one wiring's serving definition.
///
/// Params: package id, environment, wiring id. Returns at most one row —
/// `(version, wiring_hash, package_version, graph_json)` — because the
/// pointer's primary key admits exactly one enabled hash per wiring per
/// environment. The caller parses the document with
/// [`WiringDocument::parse`](crate::WiringDocument::parse) and caches it under
/// the returned hash, so this statement runs once per `(wiring, definition)` and
/// the doorbell — not a poll and not a TTL — is what makes it run again.
///
/// The exact package-membership join is deliberate: an effective release names
/// package pairs, never a compatible range. A pointer cannot keep serving a
/// wiring from a package version the current release does not contain.
///
/// The tombstone is deliberately present. A tombstone retires a wiring id
/// permanently for an environment, and it can be written after the pointer was
/// enabled; serving a retired id is the exact defect the tombstone exists to
/// prevent.
pub fn resolve_active_wiring() -> &'static str {
    "\
SELECT w.version, w.wiring_hash, w.package_version, w.graph_json::text \
  FROM catalog.wiring_activation AS x \
  JOIN catalog.wirings AS w \
    ON w.tenant_id = x.tenant_id AND w.package_id = x.package_id \
   AND w.wiring_id = x.wiring_id \
   AND w.wiring_hash = x.confirmed_definition_hash \
  JOIN catalog.effective_release_heads AS head \
    ON head.tenant_id = x.tenant_id AND head.environment = x.environment \
  JOIN catalog.effective_release_packages AS member \
    ON member.tenant_id = head.tenant_id \
   AND member.effective_release_id = head.effective_release_id \
   AND member.package_id = w.package_id \
   AND member.package_version = w.package_version \
 WHERE x.tenant_id = NULLIF(current_setting('app.tenant', true), '') \
   AND x.package_id = $1 AND x.environment = $2 AND x.wiring_id = $3 \
   AND x.enabled \
   AND NOT EXISTS ( \
       SELECT 1 FROM catalog.wiring_tombstones AS dead \
        WHERE dead.tenant_id = x.tenant_id AND dead.package_id = x.package_id \
          AND dead.environment = x.environment AND dead.wiring_id = x.wiring_id \
   )"
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        WiringActivationNotice, flip_activation, previous_confirmed_definition,
        record_activation_event, resolve_active_wiring,
    };

    fn statements() -> [&'static str; 4] {
        [
            flip_activation(),
            record_activation_event(),
            previous_confirmed_definition(),
            resolve_active_wiring(),
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

    /// The read is on the request path and must not require a row lock.
    #[test]
    fn the_env_hot_read_takes_no_row_lock_and_so_needs_no_write_grant() {
        let read = resolve_active_wiring();
        for clause in [
            "FOR UPDATE",
            "FOR SHARE",
            "FOR KEY SHARE",
            "FOR NO KEY UPDATE",
        ] {
            assert!(
                !read.contains(clause),
                "the env-hot read must not take {clause}"
            );
        }
    }

    /// The read admits only the package pair named by the effective release.
    #[test]
    fn the_env_hot_read_requires_exact_package_membership() {
        let read = resolve_active_wiring();
        assert!(read.contains("effective_release_packages"));
        assert!(
            read.contains("AND x.enabled"),
            "a disabled pointer must resolve to nothing — that is what taking a wiring dark means"
        );
        assert!(
            read.contains("FROM catalog.wiring_tombstones AS dead"),
            "a retired wiring id must stop resolving even though its pointer row survives"
        );
    }

    /// The notification uses the package-local identity directly.
    #[test]
    fn the_notice_shape_is_exactly_the_payload_the_ddl_builds() {
        let payload = json!({
            "tenant-id": "t1",
            "package-id": "shop",
            "environment": "prod",
            "wiring-id": "orders-create",
            "enabled": true,
            "confirmed-definition-hash": "sha256:00",
        });
        let notice: WiringActivationNotice =
            serde_json::from_value(payload.clone()).expect("the DDL's payload parses");
        assert_eq!(notice.package_id, "shop");
        assert_eq!(notice.wiring_id, "orders-create");
        assert_eq!(
            serde_json::to_value(&notice).expect("serializes"),
            payload,
            "the notice round-trips the exact keys json_build_object emits"
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
