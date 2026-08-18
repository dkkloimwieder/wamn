//! Storage guard tying [`WiringDocument`] to the relations that persist it.
//!
//! `deploy/sql/catalog-schema.sql` is the schema of record and cannot be
//! executed from a unit test, so these assertions pin the DDL text of the four
//! wiring relations: the tenant-isolation preamble every neighbour carries, the
//! activation trigger's load-bearing predicates (the join through
//! `catalog_heads.applied_catalog_version`, the tombstone, the
//! `wiring-definition-not-current` refusal), and the key that makes "exactly one
//! enabled definition hash per environment" structural.
//!
//! It is a text guard, not a behavioural one: it proves the refusal is *written*
//! where wamn-0h0g.18.1 says it is. Proving it *fires* needs a live PostgreSQL
//! and belongs with the `*_live.rs` gates that already provision one.

use std::collections::BTreeMap;

use wamn_catalog::{WiringDocument, WiringNode};

const SCHEMA: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");

const WIRING_RELATIONS: [&str; 4] = [
    "wirings",
    "wiring_tombstones",
    "wiring_activation",
    "wiring_activation_events",
];

/// The DDL from `start` up to the first `end` after it.
fn section(start: &str, end: &str) -> &'static str {
    let from = SCHEMA
        .find(start)
        .unwrap_or_else(|| panic!("catalog-schema.sql declares {start:?}"));
    let rest = &SCHEMA[from..];
    let to = rest
        .find(end)
        .unwrap_or_else(|| panic!("{start:?} is terminated by {end:?}"));
    &rest[..to + end.len()]
}

fn assert_declares(body: &str, fragment: &str, why: &str) {
    assert!(body.contains(fragment), "{why}: expected {fragment:?}");
}

/// A relation that skips the preamble its neighbours carry is a security defect,
/// not a style difference, so every wiring relation is checked for all of it.
#[test]
fn every_wiring_relation_is_tenant_isolated_and_read_only_to_the_app_role() {
    for relation in WIRING_RELATIONS {
        let table = section(&format!("CREATE TABLE catalog.{relation} ("), "\n);");
        assert_declares(
            table,
            "text NOT NULL CHECK (tenant_id <> '')",
            &format!("catalog.{relation} must forbid the ''-tenant row"),
        );

        let policy = section(
            &format!("CREATE POLICY {relation}_tenant ON catalog.{relation}"),
            ";",
        );
        for half in ["USING", "WITH CHECK"] {
            assert_declares(
                policy,
                &format!("{half} (tenant_id = NULLIF(current_setting('app.tenant', true), ''))"),
                &format!("catalog.{relation}'s policy must key on the app.tenant claim"),
            );
        }

        for required in [
            format!("ALTER TABLE catalog.{relation} ENABLE ROW LEVEL SECURITY;"),
            format!("ALTER TABLE catalog.{relation} FORCE ROW LEVEL SECURITY;"),
            format!("GRANT SELECT ON catalog.{relation} TO wamn_app;"),
        ] {
            assert_declares(
                SCHEMA,
                &required,
                &format!("catalog.{relation} is missing its RLS/GRANT preamble"),
            );
        }
        assert!(
            !SCHEMA.contains(&format!("INSERT ON catalog.{relation} TO")),
            "catalog.{relation} is written by the publish shell, never by a granted role"
        );
    }
}

/// The template's refusal, on this bead's relation: a pointer may only be
/// enabled onto a definition gated against the version this environment has
/// actually applied.
#[test]
fn activation_confirms_a_definition_gated_against_the_applied_catalog_version() {
    let body = section(
        "CREATE FUNCTION catalog.validate_wiring_activation()",
        "$$;",
    );

    for predicate in [
        "FROM catalog.catalog_heads head",
        "JOIN catalog.wirings wiring",
        "AND wiring.gated_catalog_version = head.applied_catalog_version",
        "AND head.environment = NEW.environment",
        "AND wiring.wiring_id = NEW.wiring_id",
        "AND wiring.wiring_hash = NEW.confirmed_definition_hash",
        "SELECT 1 FROM catalog.wiring_tombstones dead",
        "MESSAGE = 'wiring-definition-not-current'",
    ] {
        assert_declares(
            body,
            predicate,
            "the wiring activation guard lost a load-bearing predicate",
        );
    }
    assert_declares(
        body,
        "IF NOT NEW.enabled THEN\n        RETURN NEW;",
        "disabling a pointer must never be refused — that is the rollback path",
    );
    assert_declares(
        SCHEMA,
        "CREATE TRIGGER wiring_activation_valid\n\
         BEFORE INSERT OR UPDATE ON catalog.wiring_activation\n\
         FOR EACH ROW EXECUTE FUNCTION catalog.validate_wiring_activation();",
        "the guard must run on both the first flip and every later one",
    );
}

/// The doorbell is what makes activation seconds rather than a rollout, so the
/// events it must ring on are as load-bearing as the guard's predicates.
/// `AFTER INSERT` alone would ring the first activation and silence every
/// rollback, which is the one flip an operator is watching the clock on.
#[test]
fn the_doorbell_rings_on_the_first_flip_and_on_every_later_one() {
    assert_declares(
        SCHEMA,
        "CREATE TRIGGER wiring_activation_doorbell\n\
         AFTER INSERT OR UPDATE ON catalog.wiring_activation\n\
         FOR EACH ROW EXECUTE FUNCTION catalog.notify_wiring_activation();",
        "a pointer must not be able to move without ringing",
    );

    let body = section("CREATE FUNCTION catalog.notify_wiring_activation()", "$$;");
    assert_declares(
        body,
        "PERFORM pg_notify(",
        "the doorbell must ride the flip's own transaction, so it is delivered \
         on commit and never for a flip that rolled back",
    );
    for carried in [
        "NEW.wiring_id",
        "NEW.enabled",
        "NEW.confirmed_definition_hash",
    ] {
        assert_declares(
            body,
            carried,
            "the payload must name the pointer's new state, or a listener has to \
             re-read to learn what changed",
        );
    }
}

/// `catalog-schema.sql` applies whole only on a fresh install; an existing
/// project database sees the converge path in `ensure_catalog_storage`. Without
/// the markers and the probe below, every wiring relation — and the doorbell on
/// it — would reach new databases only.
#[test]
fn the_wiring_relations_reach_an_existing_database_through_the_converge_path() {
    const PUBLISHER: &str = include_str!("../../../../services/ctl/src/publish_catalog.rs");

    let begin = SCHEMA
        .find("-- BEGIN WIRING STORAGE MIGRATION")
        .expect("the wiring block is delimited for the converge path");
    let end = SCHEMA
        .find("-- END WIRING STORAGE MIGRATION")
        .expect("the wiring block's converge slice is terminated");
    let slice = &SCHEMA[begin..end];
    for relation in WIRING_RELATIONS {
        assert_declares(
            slice,
            &format!("CREATE TABLE catalog.{relation} ("),
            "the converge slice must carry every wiring relation",
        );
        assert_declares(
            PUBLISHER,
            &format!("to_regclass('catalog.{relation}') IS NOT NULL"),
            "the converge probe must notice a database missing this relation",
        );
    }
    assert_declares(
        slice,
        "CREATE TRIGGER wiring_activation_doorbell",
        "the doorbell installs with the pointer it rings for, not separately",
    );
    assert_declares(
        PUBLISHER,
        "-- BEGIN WIRING STORAGE MIGRATION",
        "the converge path must execute the slice these markers delimit",
    );
}

#[test]
fn one_environment_holds_at_most_one_enabled_hash_per_wiring() {
    let table = section("CREATE TABLE catalog.wiring_activation (", "\n);");

    assert_declares(
        table,
        "PRIMARY KEY (tenant_id, catalog_id, environment, wiring_id)",
        "the pointer key IS the exactly-one-definition-hash rule",
    );
    assert_declares(
        table,
        "confirmed_definition_hash text NOT NULL",
        "an activation that confirms no definition hash is not a confirmation",
    );
}

#[test]
fn a_definition_row_is_immutable_and_pins_a_real_catalog_version() {
    let table = section("CREATE TABLE catalog.wirings (", "\n);");

    assert_declares(
        table,
        "FOREIGN KEY (tenant_id, catalog_id, gated_catalog_version)\n\
        \x20       REFERENCES catalog.catalogs (tenant_id, catalog_id, version)",
        "the gated-against version must be a row, not an assertion",
    );
    assert_declares(
        table,
        "PRIMARY KEY (tenant_id, catalog_id, wiring_id, version)",
        "a wiring id shared by two catalogs must not resolve across them",
    );

    for (trigger, event) in [
        ("wirings_immutable", "BEFORE UPDATE ON catalog.wirings"),
        (
            "wirings_delete_immutable",
            "BEFORE DELETE ON catalog.wirings",
        ),
        (
            "wiring_activation_events_immutable",
            "BEFORE UPDATE OR DELETE ON catalog.wiring_activation_events",
        ),
    ] {
        assert_declares(
            SCHEMA,
            &format!(
                "CREATE TRIGGER {trigger}\n{event}\n\
                 FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();"
            ),
            "a gated definition and its provenance are never rewritten",
        );
    }
}

/// The column stores what the document derives — the one end-to-end fact the
/// two halves of this bead share.
#[test]
fn the_documents_derived_hash_satisfies_the_stored_columns_shape() {
    let document = WiringDocument::new(
        "orders-create",
        1,
        BTreeMap::from([(
            "in".to_owned(),
            WiringNode {
                component: "http-entry".to_owned(),
                interface_version: "0.1.0".to_owned(),
                operation: "handle".to_owned(),
                params: BTreeMap::new(),
            },
        )]),
        Vec::new(),
        Vec::new(),
    )
    .expect("a single-node wiring is a valid document");

    assert_declares(
        section("CREATE TABLE catalog.wirings (", "\n);"),
        "CHECK (wiring_hash ~ '^sha256:[0-9a-f]{64}$')",
        "the stored hash column pins the digest spelling",
    );

    let hash = document.wiring_hash().to_string();
    let hex = hash
        .strip_prefix("sha256:")
        .expect("the derived hash carries the prefix the column requires");
    assert_eq!(hex.len(), 64);
    assert!(
        hex.bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "the derived hash is lowercase hex, as the column CHECK requires"
    );
}
