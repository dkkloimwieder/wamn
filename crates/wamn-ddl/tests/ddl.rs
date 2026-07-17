//! Compiler tests over the canonical POC catalog (reused from wamn-catalog's
//! fixtures): the CREATE plan is all-additive and tenant-safe, diffs classify
//! additive vs destructive, and the safety gate refuses unconfirmed destructive
//! DDL. An optional live-apply test runs the emitted SQL against a throwaway
//! Postgres when `WAMN_DDL_PG_URL` is set.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use wamn_catalog::{Catalog, Constraint, Entity, Field, FieldType, Index};
use wamn_ddl::{CompileError, Confirmation, Migration, OutboxOptions};

/// The POC catalog fixture lives in the sibling wamn-catalog crate.
fn poc_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../wamn-catalog/tests/fixtures/poc-receiving.catalog.json")
}

fn poc() -> Catalog {
    let raw = std::fs::read_to_string(poc_fixture()).expect("read POC fixture");
    Catalog::from_json(&raw).expect("POC fixture parses")
}

fn text_field(id: &str) -> Field {
    Field {
        id: id.into(),
        name: id.into(),
        field_type: FieldType::Text { max_len: None },
        nullable: true,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    }
}

/// A minimal user entity for the name-reuse tests.
fn entity(id: &str, name: &str, fields: Vec<Field>) -> Entity {
    Entity {
        id: id.into(),
        name: name.into(),
        is_system: false,
        label: None,
        description: None,
        fields,
        indexes: vec![],
        constraints: vec![],
    }
}

fn mini(version: u32, entities: Vec<Entity>) -> Catalog {
    Catalog {
        schema_version: "0.1".into(),
        catalog_id: "k56-name-reuse".into(),
        version,
        name: None,
        entities,
        relations: vec![],
    }
}

fn reference_field(id: &str, target_entity: &str) -> Field {
    Field {
        id: id.into(),
        name: id.into(),
        field_type: FieldType::Reference {
            entity: target_entity.into(),
        },
        nullable: true,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    }
}

/// A plain `uuid` field — the retyped-away-from/into shape for the FK-lifecycle
/// tests (a `reference` and a `uuid` are both `uuid` columns; only the FK
/// differs).
fn uuid_field(id: &str) -> Field {
    Field {
        id: id.into(),
        name: id.into(),
        field_type: FieldType::Uuid,
        nullable: true,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    }
}

#[test]
fn create_plan_is_additive_and_tenant_safe() {
    let plan = Migration::create(&poc()).expect("compiles");
    assert!(!plan.is_empty());
    assert!(plan.is_additive(), "a fresh CREATE has no destructive ops");
    assert!(!plan.requires_confirmation());

    let sql = plan
        .sql(Confirmation::None)
        .expect("additive needs no confirmation");
    // Tenant floor + managed PK.
    assert!(sql.contains("CREATE TABLE \"receipts\""));
    assert!(sql.contains("id uuid PRIMARY KEY DEFAULT gen_random_uuid()"));
    // Tenant column is NOT NULL and structurally non-empty (an empty tenant_id
    // collides with the '' a reset GUC carries — see wamn-a45).
    assert!(sql.contains("tenant_id text NOT NULL CHECK (tenant_id <> '')"));
    assert!(sql.contains("FORCE ROW LEVEL SECURITY"));
    // The policy reads the claim through NULLIF so an empty claim => NULL =>
    // matches no row, in BOTH the USING and WITH CHECK clauses.
    assert!(sql.contains("USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))"));
    assert!(
        sql.contains("WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''))")
    );
    assert!(sql.contains("GRANT SELECT, INSERT, UPDATE, DELETE ON \"receipts\" TO wamn_app"));
    // Composite unique is tenant-scoped.
    assert!(sql.contains("UNIQUE (tenant_id, \"receipt_no\", \"supplier_id\")"));
    // Exact-decimal + unit comment; enum -> text + CHECK.
    assert!(sql.contains("numeric(5,2)"));
    assert!(sql.contains("IS 'unit: pct'"));
    assert!(sql.contains("CHECK (\"status\" IN ('open', 'disposed', 'escalated'))"));
    // Reference -> uuid column + FK.
    assert!(sql.contains("FOREIGN KEY (\"supplier_id\") REFERENCES \"suppliers\" (id)"));
}

/// A field of an arbitrary type (for the special-value CHECK test).
fn field_of(id: &str, field_type: FieldType) -> Field {
    Field {
        id: id.into(),
        name: id.into(),
        field_type,
        nullable: true,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    }
}

/// The floor forbids the JSON-type-changing special values (`NaN` on numeric,
/// `+/-infinity` on date/timestamptz) that `to_jsonb` would serialize as JSON
/// strings in a row-event outbox payload (wamn-oj7). Covers both `CREATE TABLE`
/// (here) and `ADD COLUMN` (both call `column_clause`).
#[test]
fn numeric_and_timestamp_columns_exclude_special_values() {
    let readings = entity(
        "er",
        "readings",
        vec![
            field_of(
                "qty",
                FieldType::Numeric {
                    precision: 10,
                    scale: 2,
                    unit: None,
                },
            ),
            field_of("d", FieldType::Date),
            field_of("ts", FieldType::Timestamptz),
        ],
    );
    let sql = Migration::create(&mini(1, vec![readings]))
        .expect("compiles")
        .sql(Confirmation::None)
        .expect("additive");

    assert!(
        sql.contains("CHECK (\"qty\" <> 'NaN'::numeric)"),
        "numeric column forbids NaN:\n{sql}"
    );
    assert!(
        sql.contains("CHECK (\"d\" <> 'infinity'::date AND \"d\" <> '-infinity'::date)"),
        "date column forbids +/-infinity:\n{sql}"
    );
    assert!(
        sql.contains(
            "CHECK (\"ts\" <> 'infinity'::timestamptz AND \"ts\" <> '-infinity'::timestamptz)"
        ),
        "timestamptz column forbids +/-infinity:\n{sql}"
    );
    // The base column types survive unchanged (the CHECK is appended).
    assert!(sql.contains("\"qty\" numeric(10,2)"));
    assert!(sql.contains("\"d\" date"));
    assert!(sql.contains("\"ts\" timestamptz"));
}

#[test]
fn added_column_is_additive() {
    let v1 = poc();
    let mut v2 = v1.clone();
    v2.version = 2;
    let materials = v2
        .entities
        .iter_mut()
        .find(|e| e.id == "materials")
        .unwrap();
    materials.fields.push(text_field("grade"));
    materials.indexes.push(Index {
        name: "materials_grade_idx".into(),
        fields: vec!["grade".into()],
        unique: false,
    });

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    assert!(plan.is_additive(), "report: {}", plan.report());
    let sql = plan.sql(Confirmation::None).expect("additive");
    assert!(sql.contains("ALTER TABLE \"materials\" ADD COLUMN \"grade\" text"));
    assert!(sql.contains("CREATE INDEX \"materials_grade_idx\""));
}

#[test]
fn dropped_column_is_gated_destructive() {
    let v1 = poc();
    let mut v2 = v1.clone();
    v2.version = 2;
    let suppliers = v2
        .entities
        .iter_mut()
        .find(|e| e.id == "suppliers")
        .unwrap();
    suppliers.fields.retain(|f| f.id != "contact_email");

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    assert!(plan.requires_confirmation());
    assert_eq!(plan.destructive().count(), 1);

    // Refused without confirmation…
    let err = plan.sql(Confirmation::None).unwrap_err();
    assert!(err.destructive.iter().any(|s| s.contains("drop column")));

    // …allowed with confirmation + backup, and marked.
    let sql = plan
        .sql(Confirmation::ConfirmedWithBackup)
        .expect("confirmed");
    assert!(sql.contains("BACKUP CHECKPOINT REQUIRED"));
    assert!(sql.contains("ALTER TABLE \"suppliers\" DROP COLUMN \"contact_email\""));
}

#[test]
fn renamed_field_is_destructive() {
    // The 11.8 impact case: staging quality_holds.status -> hold_status.
    let v1 = poc();
    let mut v2 = v1.clone();
    v2.version = 2;
    let holds = v2
        .entities
        .iter_mut()
        .find(|e| e.id == "quality_holds")
        .unwrap();
    holds
        .fields
        .iter_mut()
        .find(|f| f.id == "status")
        .unwrap()
        .name = "hold_status".into();

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    assert!(plan.requires_confirmation());
    let op = plan
        .operations
        .iter()
        .find(|o| o.summary.contains("rename column"))
        .expect("a rename op");
    assert!(
        op.sql
            .contains("RENAME COLUMN \"status\" TO \"hold_status\"")
    );
    assert_eq!(op.entity, "quality_holds");
    assert_eq!(op.field.as_deref(), Some("status"));
}

#[test]
fn reused_name_via_rename_is_freed_before_create() {
    // v1{B:'receipts'} -> v2{B -> 'receipts_old', NEW D named 'receipts'}: the
    // name-freeing rename must precede the CREATE that reclaims the name —
    // 42P07 otherwise, and the whole 2.5 transactional apply rolls back.
    let v1 = mini(1, vec![entity("b", "receipts", vec![text_field("val")])]);
    let v2 = mini(
        2,
        vec![
            entity("b", "receipts_old", vec![text_field("val")]),
            entity("d", "receipts", vec![text_field("other")]),
        ],
    );
    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    assert!(plan.requires_confirmation(), "rename stays gated");
    let sql = plan
        .sql(Confirmation::ConfirmedWithBackup)
        .expect("confirmed");
    let rename = sql
        .find("ALTER TABLE \"receipts\" RENAME TO \"receipts_old\"")
        .expect("rename op");
    let create = sql.find("CREATE TABLE \"receipts\"").expect("create op");
    assert!(
        rename < create,
        "the name-freeing rename must precede the CREATE reclaiming it:\n{sql}"
    );
}

#[test]
fn reused_name_via_drop_renames_aside_and_drops_last() {
    // Remove-and-re-add of 'audit' in one bump. The doomed table is renamed
    // aside — WITH its indexes: index names (incl. the implicit pkey) do not
    // follow a table rename, so the recreated table's canonical index names
    // would otherwise collide or drift. The DROP TABLE stays LAST, so the FK
    // unwind of the destructive tail is untouched: log's reference column
    // (the inbound FK on the doomed table) drops before the aside table does.
    let mut x = entity("x", "audit", vec![text_field("val")]);
    x.indexes.push(Index {
        name: "audit_by_val".into(),
        fields: vec!["val".into()],
        unique: false,
    });
    x.constraints.push(Constraint::Unique {
        name: "audit_val_uniq".into(),
        fields: vec!["val".into()],
    });
    let mut y1 = entity("y", "log", vec![text_field("msg")]);
    y1.fields.push(reference_field("audit_ref", "x"));
    let v1 = mini(1, vec![x.clone(), y1]);

    let mut e = x.clone();
    e.id = "e".into();
    let v2 = mini(2, vec![e, entity("y", "log", vec![text_field("msg")])]);

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan
        .sql(Confirmation::ConfirmedWithBackup)
        .expect("confirmed");

    let aside = sql
        .find("ALTER TABLE \"audit\" RENAME TO \"wamn_mig_drop_audit\"")
        .expect("aside rename");
    let pkey = sql
        .find("ALTER INDEX IF EXISTS \"audit_pkey\" RENAME TO \"wamn_mig_drop_audit_pkey\"")
        .expect("pkey moved aside");
    let idx = sql
        .find("ALTER INDEX IF EXISTS \"audit_by_val\"")
        .expect("index moved aside");
    let uniq = sql
        .find("ALTER INDEX IF EXISTS \"audit_val_uniq\"")
        .expect("unique backing index moved aside");
    let create = sql.find("CREATE TABLE \"audit\"").expect("create");
    let fk_unwind = sql
        .find("ALTER TABLE \"log\" DROP COLUMN \"audit_ref\"")
        .expect("inbound FK column drop");
    let final_drop = sql
        .find("DROP TABLE \"wamn_mig_drop_audit\"")
        .expect("final drop targets the aside name");
    assert!(aside < create && pkey < create && idx < create && uniq < create);
    assert!(
        create < fk_unwind && fk_unwind < final_drop,
        "FK unwind must precede the aside table's final drop:\n{sql}"
    );
    // The review surface keeps the real table name.
    let drop_op = plan
        .operations
        .iter()
        .find(|o| o.sql.starts_with("DROP TABLE"))
        .unwrap();
    assert_eq!(drop_op.summary, "drop table audit");
    assert!(!sql.contains("DROP TABLE \"audit\""));
}

#[test]
fn rename_chain_orders_name_freeing_first() {
    // a -> b, b -> c, c -> d in one bump (diff order deliberately worst-case):
    // each rename's target is freed by the next, so the emission order must be
    // fully reversed — a single ready/blocked pass is not enough (the blocked
    // remainder must be re-ordered too, which needs the full loop).
    let v1 = mini(
        1,
        vec![
            entity("ea", "a", vec![text_field("v")]),
            entity("eb", "b", vec![text_field("v")]),
            entity("ec", "c", vec![text_field("v")]),
        ],
    );
    let v2 = mini(
        2,
        vec![
            entity("ea", "b", vec![text_field("v")]),
            entity("eb", "c", vec![text_field("v")]),
            entity("ec", "d", vec![text_field("v")]),
        ],
    );
    let plan = Migration::migrate(&v1, &v2).expect("a chain compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let free_d = sql
        .find("ALTER TABLE \"c\" RENAME TO \"d\"")
        .expect("tail rename");
    let free_c = sql
        .find("ALTER TABLE \"b\" RENAME TO \"c\"")
        .expect("middle rename");
    let claim_b = sql
        .find("ALTER TABLE \"a\" RENAME TO \"b\"")
        .expect("head rename");
    assert!(
        free_d < free_c && free_c < claim_b,
        "the chain must emit fully reversed (freeing first):\n{sql}"
    );
}

#[test]
fn pkey_follows_a_table_rename() {
    // Index names do not follow a table rename. Left stale, the recreated
    // table in the headline scenario silently gets a suffixed pkey
    // (receipts_pkey1 — Postgres auto-avoids rather than erroring), and a
    // LATER migration's aside-rename of "receipts_pkey" would grab the LIVE
    // renamed table's index. Each hoisted rename takes its pkey along.
    let v1 = mini(1, vec![entity("b", "receipts", vec![text_field("val")])]);
    let v2 = mini(
        2,
        vec![
            entity("b", "receipts_old", vec![text_field("val")]),
            entity("d", "receipts", vec![text_field("other")]),
        ],
    );
    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let rename = sql
        .find("ALTER TABLE \"receipts\" RENAME TO \"receipts_old\"")
        .expect("table rename");
    let pkey = sql
        .find("ALTER INDEX IF EXISTS \"receipts_pkey\" RENAME TO \"receipts_old_pkey\"")
        .expect("pkey follows the rename");
    let create = sql.find("CREATE TABLE \"receipts\"").expect("create");
    assert!(
        rename < pkey && pkey < create,
        "the pkey rename must free the canonical name before the CREATE:\n{sql}"
    );
}

#[test]
fn removed_entity_drops_are_fk_ordered() {
    // v1: authors <- books (books.author_id references authors). v2 drops both.
    // `entities_removed` arrives id-lexical (a_authors < b_books), so a
    // dependency-blind emission would DROP "authors" before "books" — 2BP01,
    // the FK constraint on books depends on authors, and the one-txn apply
    // rolls back. The topological pass must emit the dependent child first.
    let authors = entity("a_authors", "authors", vec![text_field("name")]);
    let mut books = entity("b_books", "books", vec![text_field("title")]);
    books.fields.push(reference_field("author_id", "a_authors"));
    let v1 = mini(1, vec![authors, books]);
    let v2 = mini(2, vec![]);

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    // Both table drops are destructive, so the whole plan is gated.
    assert!(plan.requires_confirmation());
    assert!(plan.sql(Confirmation::None).is_err());
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let drop_books = sql.find("DROP TABLE \"books\"").expect("drop books");
    let drop_authors = sql.find("DROP TABLE \"authors\"").expect("drop authors");
    assert!(
        drop_books < drop_authors,
        "the referencing child must drop before the referenced parent:\n{sql}"
    );
}

#[test]
fn removed_entity_drop_chain_orders_dependents_first() {
    // A 3-table chain, authors <- books <- reviews (reviews -> books -> authors),
    // all dropped in one bump. Ids are chosen so lexical order (e1, e2, e3) is
    // the exact REVERSE of the safe drop order (reviews, books, authors) — a
    // single ready/blocked pass would pull only the leaf out and leave the rest
    // lexical, so the full Kahn loop is required.
    let authors = entity("e1_root", "authors", vec![text_field("name")]);
    let mut books = entity("e2_mid", "books", vec![text_field("title")]);
    books.fields.push(reference_field("author_id", "e1_root"));
    let mut reviews = entity("e3_leaf", "reviews", vec![text_field("body")]);
    reviews.fields.push(reference_field("book_id", "e2_mid"));
    let v1 = mini(1, vec![authors, books, reviews]);
    let v2 = mini(2, vec![]);

    let plan = Migration::migrate(&v1, &v2).expect("a drop chain compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let drop_reviews = sql.find("DROP TABLE \"reviews\"").expect("drop reviews");
    let drop_books = sql.find("DROP TABLE \"books\"").expect("drop books");
    let drop_authors = sql.find("DROP TABLE \"authors\"").expect("drop authors");
    assert!(
        drop_reviews < drop_books && drop_books < drop_authors,
        "the chain must drop fully dependents-first (leaf -> root):\n{sql}"
    );
}

#[test]
fn mutual_fk_among_dropped_tables_is_rejected() {
    // Two tables each referencing the other, both dropped in one bump: no
    // DROP TABLE order unwinds the FKs without a prior constraint drop, so v1
    // rejects it (as the rename/column cycles are rejected) rather than emit a
    // plan that cannot apply.
    let mut a = entity("ea", "left", vec![text_field("v")]);
    a.fields.push(reference_field("right_ref", "eb"));
    let mut b = entity("eb", "right", vec![text_field("v")]);
    b.fields.push(reference_field("left_ref", "ea"));
    let v1 = mini(1, vec![a, b]);
    let v2 = mini(2, vec![]);

    match Migration::migrate(&v1, &v2) {
        Err(CompileError::DropCycle { entities }) => {
            assert_eq!(
                entities.len(),
                2,
                "both cycle members reported: {entities:?}"
            );
            assert!(entities.iter().any(|e| e == "ea"));
            assert!(entities.iter().any(|e| e == "eb"));
        }
        other => panic!("expected DropCycle, got {other:?}"),
    }
}

#[test]
fn reference_retype_to_uuid_drops_the_fk() {
    // v1: audit + log(aref -> audit). v2: audit removed AND log.aref retyped
    // from Reference to Uuid (same field id). The synthesized FK log_aref_fkey
    // is not carried by ALTER COLUMN TYPE, so without an explicit drop it
    // survives the retype and the later DROP TABLE "audit" fails 2BP01. The
    // drop must precede the table drop.
    let audit = entity("x", "audit", vec![text_field("val")]);
    let mut log1 = entity("y", "log", vec![text_field("msg")]);
    log1.fields.push(reference_field("aref", "x"));
    let v1 = mini(1, vec![audit, log1]);

    let mut log2 = entity("y", "log", vec![text_field("msg")]);
    log2.fields.push(uuid_field("aref"));
    let v2 = mini(2, vec![log2]);

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    assert!(plan.requires_confirmation());
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let drop_fk = sql
        .find("ALTER TABLE \"log\" DROP CONSTRAINT \"log_aref_fkey\"")
        .expect("stale reference FK dropped");
    let drop_tbl = sql
        .find("DROP TABLE \"audit\"")
        .expect("target table dropped");
    assert!(
        drop_fk < drop_tbl,
        "the FK must drop before its referenced table:\n{sql}"
    );
}

#[test]
fn retype_into_reference_adds_the_fk() {
    // The converse: log.aref retyped from Uuid to Reference{audit}. No FK
    // existed, so one must be ADDED (after the retype makes the column uuid),
    // and nothing is dropped.
    let audit = entity("x", "audit", vec![text_field("val")]);
    let mut log1 = entity("y", "log", vec![text_field("msg")]);
    log1.fields.push(uuid_field("aref"));
    let v1 = mini(1, vec![audit.clone(), log1]);

    let mut log2 = entity("y", "log", vec![text_field("msg")]);
    log2.fields.push(reference_field("aref", "x"));
    let v2 = mini(2, vec![audit, log2]);

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let retype = sql
        .find("ALTER TABLE \"log\" ALTER COLUMN \"aref\" TYPE uuid")
        .expect("retype");
    let add_fk = sql
        .find(
            "ALTER TABLE \"log\" ADD CONSTRAINT \"log_aref_fkey\" \
             FOREIGN KEY (\"aref\") REFERENCES \"audit\" (id)",
        )
        .expect("FK added on entering Reference");
    assert!(
        retype < add_fk,
        "the FK is added after the column becomes uuid:\n{sql}"
    );
    assert!(
        !sql.contains("DROP CONSTRAINT \"log_aref_fkey\""),
        "nothing to drop when entering Reference:\n{sql}"
    );
}

#[test]
fn reference_retype_repoints_the_fk() {
    // A Reference re-pointed at a NEW target (authors -> books) does both:
    // drop the old-named FK, then add the new FK referencing books.
    let authors = entity("a", "authors", vec![text_field("n")]);
    let books = entity("b", "books", vec![text_field("t")]);
    let mut log1 = entity("y", "log", vec![text_field("msg")]);
    log1.fields.push(reference_field("aref", "a"));
    let v1 = mini(1, vec![authors.clone(), books.clone(), log1]);

    let mut log2 = entity("y", "log", vec![text_field("msg")]);
    log2.fields.push(reference_field("aref", "b"));
    let v2 = mini(2, vec![authors, books, log2]);

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let drop_fk = sql
        .find("ALTER TABLE \"log\" DROP CONSTRAINT \"log_aref_fkey\"")
        .expect("old FK dropped");
    let add_fk = sql
        .find(
            "ALTER TABLE \"log\" ADD CONSTRAINT \"log_aref_fkey\" \
             FOREIGN KEY (\"aref\") REFERENCES \"books\" (id)",
        )
        .expect("new FK references books");
    assert!(
        drop_fk < add_fk,
        "the re-point drops the old FK before adding the new one:\n{sql}"
    );
}

#[test]
fn reused_name_via_drop_reclaimed_by_a_rename() {
    // The freed name is reclaimed by a RENAME, not a CREATE — the rename
    // TARGETS (not sources) must drive the claim analysis: v1{x:'n' removed,
    // y:'y_old'} -> v2{y renamed to 'n'}. The doomed x must move aside before
    // the rename, and the final drop must target the aside name.
    let v1 = mini(
        1,
        vec![
            entity("x", "n", vec![text_field("v")]),
            entity("y", "y_old", vec![text_field("v")]),
        ],
    );
    let v2 = mini(2, vec![entity("y", "n", vec![text_field("v")])]);
    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let aside = sql
        .find("ALTER TABLE \"n\" RENAME TO \"wamn_mig_drop_n\"")
        .expect("doomed table moved aside");
    let rename = sql
        .find("ALTER TABLE \"y_old\" RENAME TO \"n\"")
        .expect("claiming rename");
    let final_drop = sql
        .find("DROP TABLE \"wamn_mig_drop_n\"")
        .expect("final drop targets the aside name");
    assert!(
        aside < rename && rename < final_drop,
        "aside first, then the claiming rename:\n{sql}"
    );
}

#[test]
fn doomed_table_keeping_its_name_moves_reclaimed_index_names_aside() {
    // The doomed table's NAME is not reused, but a NEW table claims its index
    // and unique-constraint names — they alone must move aside; the table
    // keeps its name until the final drop.
    let mut x = entity("x", "audit", vec![text_field("v")]);
    x.indexes.push(Index {
        name: "hot_idx".into(),
        fields: vec!["v".into()],
        unique: false,
    });
    x.constraints.push(Constraint::Unique {
        name: "uq_shared".into(),
        fields: vec!["v".into()],
    });
    let v1 = mini(1, vec![x]);
    let mut m = entity("m", "metrics", vec![text_field("v")]);
    m.indexes.push(Index {
        name: "hot_idx".into(),
        fields: vec!["v".into()],
        unique: false,
    });
    m.constraints.push(Constraint::Unique {
        name: "uq_shared".into(),
        fields: vec!["v".into()],
    });
    let v2 = mini(2, vec![m]);

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let idx_aside = sql
        .find("ALTER INDEX IF EXISTS \"hot_idx\" RENAME TO \"wamn_mig_drop_hot_idx\"")
        .expect("reclaimed index moved aside");
    let uq_aside = sql
        .find("ALTER INDEX IF EXISTS \"uq_shared\" RENAME TO \"wamn_mig_drop_uq_shared\"")
        .expect("reclaimed unique backing index moved aside");
    let claim = sql
        .find("CREATE INDEX \"hot_idx\"")
        .expect("claiming index add");
    let uq_claim = sql
        .find("ADD CONSTRAINT \"uq_shared\"")
        .expect("claiming constraint add");
    assert!(idx_aside < claim && uq_aside < uq_claim);
    // The table itself keeps its real name to the end.
    assert!(sql.contains("DROP TABLE \"audit\""));
    assert!(!sql.contains("ALTER TABLE \"audit\" RENAME"));
}

#[test]
fn unique_constraint_name_moving_across_tables_drops_before_add() {
    // A UNIQUE constraint name migrating from one CHANGED entity to another:
    // the per-entity re-add set cannot see it — only the cross-entity claimed
    // set can — and the freeing drop must precede the claiming add.
    let mut a1 = entity("ea", "alpha", vec![text_field("v")]);
    a1.constraints.push(Constraint::Unique {
        name: "uq_moved".into(),
        fields: vec!["v".into()],
    });
    let b1 = entity("eb", "beta", vec![text_field("v")]);
    let v1 = mini(1, vec![a1, b1]);

    let a2 = entity("ea", "alpha", vec![text_field("v"), text_field("w")]);
    let mut b2 = entity("eb", "beta", vec![text_field("v")]);
    b2.constraints.push(Constraint::Unique {
        name: "uq_moved".into(),
        fields: vec!["v".into()],
    });
    let v2 = mini(2, vec![a2, b2]);

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let drop = sql
        .find("ALTER TABLE \"alpha\" DROP CONSTRAINT \"uq_moved\"")
        .expect("freeing drop");
    let add = sql
        .find("ALTER TABLE \"beta\" ADD CONSTRAINT \"uq_moved\"")
        .expect("claiming add");
    assert!(
        drop < add,
        "the cross-table name move must drop first:\n{sql}"
    );
}

#[test]
fn reused_column_name_frees_before_the_add() {
    // The column-namespace sibling: dropping field id f1 (name 'amount') and
    // adding f2 with the SAME name is a valid evolution (field identity = id);
    // the DROP COLUMN must precede the ADD COLUMN (42701 otherwise). Same for
    // a column RENAME into a dropped column's name, and rename chains order
    // within the table.
    let v1 = mini(
        1,
        vec![entity(
            "t",
            "t",
            vec![text_field("amount"), text_field("k")],
        )],
    );
    let mut t2 = entity("t", "t", vec![text_field("k")]);
    t2.fields.push(Field {
        id: "amount2".into(),
        name: "amount".into(),
        field_type: FieldType::Int,
        nullable: true,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    });
    let v2 = mini(2, vec![t2]);
    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    assert!(
        sql.find("DROP COLUMN \"amount\"").unwrap() < sql.find("ADD COLUMN \"amount\"").unwrap(),
        "same-named column redefinition must drop first:\n{sql}"
    );

    // Column rename chain within a table: a -> b while b -> c (and b's old
    // name reclaimed) must emit the freeing rename first.
    let mut ta = text_field("a");
    ta.id = "fa".into();
    let mut tb = text_field("b");
    tb.id = "fb".into();
    let v3 = mini(3, vec![entity("t", "t", vec![ta, tb])]);
    let mut ta4 = text_field("b");
    ta4.id = "fa".into();
    let mut tb4 = text_field("c");
    tb4.id = "fb".into();
    let v4 = mini(4, vec![entity("t", "t", vec![ta4, tb4])]);
    let plan = Migration::migrate(&v3, &v4).expect("column chain compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let free = sql
        .find("RENAME COLUMN \"b\" TO \"c\"")
        .expect("freeing column rename");
    let claim = sql
        .find("RENAME COLUMN \"a\" TO \"b\"")
        .expect("claiming column rename");
    assert!(free < claim, "column chain orders freeing first:\n{sql}");
}

#[test]
fn implicitly_dropped_objects_hoist_with_the_column() {
    // A hoisted DROP COLUMN implicitly drops constraints/indexes involving
    // the column (verified on PG 18: a later explicit drop then errors
    // 42704), so an entity that hoists a column drop must hoist ALL its
    // constraint/index drops ahead of it — even ones that free no reused
    // name and would otherwise sit in the destructive tail.
    let mut t1 = entity("t", "t", vec![text_field("amount"), text_field("k")]);
    t1.constraints.push(Constraint::Unique {
        name: "uq_amount".into(),
        fields: vec!["amount".into()],
    });
    let v1 = mini(1, vec![t1]);
    // amount dropped-and-re-added (new field id, no constraint this time):
    // the column drop hoists; uq_amount is dropped but NOT re-added.
    let mut t2 = entity("t", "t", vec![text_field("k")]);
    t2.fields.push(Field {
        id: "amount2".into(),
        name: "amount".into(),
        field_type: FieldType::Int,
        nullable: true,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    });
    let v2 = mini(2, vec![t2]);

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let con_drop = sql
        .find("DROP CONSTRAINT \"uq_amount\"")
        .expect("constraint drop");
    let col_drop = sql.find("DROP COLUMN \"amount\"").expect("column drop");
    assert!(
        con_drop < col_drop,
        "the constraint drop must precede the column drop that would implicitly consume it:\n{sql}"
    );
}

#[test]
fn column_rename_swap_is_rejected() {
    let mut fa = text_field("a");
    fa.id = "fa".into();
    let mut fb = text_field("b");
    fb.id = "fb".into();
    let v1 = mini(1, vec![entity("t", "t", vec![fa, fb])]);
    let mut fa2 = text_field("b");
    fa2.id = "fa".into();
    let mut fb2 = text_field("a");
    fb2.id = "fb".into();
    let v2 = mini(2, vec![entity("t", "t", vec![fa2, fb2])]);
    match Migration::migrate(&v1, &v2) {
        Err(CompileError::ColumnRenameCycle { entity, names }) => {
            assert_eq!(entity, "t");
            assert!(names.contains(&"a".to_string()) && names.contains(&"b".to_string()));
        }
        other => panic!("expected ColumnRenameCycle, got {other:?}"),
    }
}

#[test]
fn rename_swap_cycle_is_rejected() {
    // a <-> b in one bump: no order of plain renames applies it — rejected,
    // split into two version bumps.
    let v1 = mini(
        1,
        vec![
            entity("ea", "a", vec![text_field("v")]),
            entity("eb", "b", vec![text_field("v")]),
        ],
    );
    let v2 = mini(
        2,
        vec![
            entity("ea", "b", vec![text_field("v")]),
            entity("eb", "a", vec![text_field("v")]),
        ],
    );
    match Migration::migrate(&v1, &v2) {
        Err(CompileError::TableRenameCycle { names }) => {
            assert!(names.contains(&"a".to_string()) && names.contains(&"b".to_string()));
        }
        other => panic!("expected TableRenameCycle, got {other:?}"),
    }
}

#[test]
fn rename_with_other_changes_renames_first() {
    // A renamed table with any other change used to emit
    // `ALTER TABLE <new name> ...` BEFORE the rename executed (42P01); the
    // hoisted rename must precede the entity's other operations.
    let v1 = mini(1, vec![entity("b", "receipts", vec![text_field("val")])]);
    let v2 = mini(
        2,
        vec![entity(
            "b",
            "receipts2",
            vec![text_field("val"), text_field("note")],
        )],
    );
    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let rename = sql
        .find("ALTER TABLE \"receipts\" RENAME TO \"receipts2\"")
        .expect("rename");
    let add = sql
        .find("ALTER TABLE \"receipts2\" ADD COLUMN \"note\" text")
        .expect("add column on the NEW name");
    assert!(
        rename < add,
        "the rename must precede other ALTERs on the renamed table:\n{sql}"
    );
}

#[test]
fn constraint_and_index_redefinition_drop_before_add() {
    // Changing a constraint's or index's definition while KEEPING its name
    // diffs as drop + add; the drop must run first (42710 / 42P07 otherwise).
    let mut e1 = entity(
        "r",
        "receipts",
        vec![text_field("val"), text_field("other")],
    );
    e1.constraints.push(Constraint::Unique {
        name: "u_val".into(),
        fields: vec!["val".into()],
    });
    e1.indexes.push(Index {
        name: "i_val".into(),
        fields: vec!["val".into()],
        unique: false,
    });
    let v1 = mini(1, vec![e1]);

    let mut e2 = entity(
        "r",
        "receipts",
        vec![text_field("val"), text_field("other")],
    );
    e2.constraints.push(Constraint::Unique {
        name: "u_val".into(),
        fields: vec!["val".into(), "other".into()],
    });
    e2.indexes.push(Index {
        name: "i_val".into(),
        fields: vec!["val".into(), "other".into()],
        unique: false,
    });
    let v2 = mini(2, vec![e2]);

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    assert!(
        sql.find("DROP CONSTRAINT \"u_val\"").unwrap()
            < sql.find("ADD CONSTRAINT \"u_val\"").unwrap(),
        "same-named constraint redefinition must drop first:\n{sql}"
    );
    assert!(
        sql.find("DROP INDEX \"i_val\"").unwrap() < sql.find("CREATE INDEX \"i_val\"").unwrap(),
        "same-named index redefinition must drop first:\n{sql}"
    );
}

#[test]
fn constraint_redefinition_on_renamed_table_drops_before_the_rename() {
    // A hoisted (name-freeing) constraint drop references its table by the
    // PRE-rename name and runs BEFORE the renames: a rename's own target may
    // be a name freed only by such a drop (a unique constraint's backing
    // index shares the relation namespace), so the drops cannot wait for the
    // renames. The re-add then references the post-rename name.
    let mut e1 = entity(
        "r",
        "receipts",
        vec![text_field("val"), text_field("other")],
    );
    e1.constraints.push(Constraint::Unique {
        name: "u_val".into(),
        fields: vec!["val".into()],
    });
    let v1 = mini(1, vec![e1]);

    let mut e2 = entity(
        "r",
        "receipts2",
        vec![text_field("val"), text_field("other")],
    );
    e2.constraints.push(Constraint::Unique {
        name: "u_val".into(),
        fields: vec!["val".into(), "other".into()],
    });
    let v2 = mini(2, vec![e2]);

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let drop = sql
        .find("ALTER TABLE \"receipts\" DROP CONSTRAINT \"u_val\"")
        .expect("hoisted drop on the PRE-rename name");
    let rename = sql
        .find("ALTER TABLE \"receipts\" RENAME TO \"receipts2\"")
        .expect("rename");
    let add = sql
        .find("ALTER TABLE \"receipts2\" ADD CONSTRAINT \"u_val\"")
        .expect("re-add on the post-rename name");
    assert!(
        drop < rename && rename < add,
        "the hoisted drop, then the rename, then the re-add:\n{sql}"
    );
}

#[test]
fn rename_into_name_freed_by_a_constraint_drop_applies() {
    // The review's headline counterexample: A renamed INTO a name freed only
    // by another table's dropped unique constraint (its backing index holds
    // the relation-namespace name). The drop must precede the rename.
    let mut b1 = entity("eb", "b", vec![text_field("v")]);
    b1.constraints.push(Constraint::Unique {
        name: "target".into(),
        fields: vec!["v".into()],
    });
    let v1 = mini(1, vec![entity("ea", "a", vec![text_field("v")]), b1]);
    let v2 = mini(
        2,
        vec![
            entity("ea", "target", vec![text_field("v")]),
            entity("eb", "b", vec![text_field("v")]),
        ],
    );
    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    let drop = sql
        .find("ALTER TABLE \"b\" DROP CONSTRAINT \"target\"")
        .expect("freeing constraint drop");
    let rename = sql
        .find("ALTER TABLE \"a\" RENAME TO \"target\"")
        .expect("claiming rename");
    assert!(
        drop < rename,
        "the constraint drop frees the rename's target name:\n{sql}"
    );
}

#[test]
fn collision_free_plans_have_no_preamble() {
    // The name-reuse machinery must not perturb ordinary migrations: an
    // add-column + drop-column evolution has no preamble ops and keeps the
    // additive-first / destructive-last shape.
    let v1 = poc();
    let mut v2 = v1.clone();
    v2.version = 2;
    let materials = v2
        .entities
        .iter_mut()
        .find(|e| e.id == "materials")
        .unwrap();
    materials.fields.push(text_field("grade"));
    let suppliers = v2
        .entities
        .iter_mut()
        .find(|e| e.id == "suppliers")
        .unwrap();
    suppliers.fields.retain(|f| f.id != "contact_email");

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    assert!(
        plan.operations
            .iter()
            .all(|o| !o.sql.contains("wamn_mig_drop_") && !o.sql.contains("RENAME TO"))
    );
    let sql = plan.sql(Confirmation::ConfirmedWithBackup).unwrap();
    assert!(
        sql.find("ADD COLUMN \"grade\"").unwrap()
            < sql.find("DROP COLUMN \"contact_email\"").unwrap(),
        "additive-first shape intact:\n{sql}"
    );
}

#[test]
fn reserved_prefix_name_is_rejected_before_the_aside_collision() {
    // wamn-66x: a user catalog can no longer carry a `wamn_`-prefixed name, so
    // the pathological aside-name collision (k56's `TempNameCollision`) never
    // reaches the migration compiler through the public `migrate()` — `check()`
    // validates first and `validate()` rejects the reserved prefix with
    // `reserved-name-prefix`. `TempNameCollision` survives as defense-in-depth on
    // the internal, unvalidated `migrate_plan` (covered by the emit.rs unit test
    // `migrate_plan_rejects_aside_name_collision`).
    let v1 = mini(
        1,
        vec![
            entity("x", "audit", vec![text_field("v")]),
            entity("t", "wamn_mig_drop_audit", vec![text_field("v")]),
        ],
    );
    let v2 = mini(
        2,
        vec![
            entity("e", "audit", vec![text_field("v")]),
            entity("t", "wamn_mig_drop_audit", vec![text_field("v")]),
        ],
    );
    match Migration::migrate(&v1, &v2) {
        Err(CompileError::InvalidCatalog(issues)) => {
            assert!(
                issues.iter().any(|i| i.code == "reserved-name-prefix"
                    && i.message.contains("wamn_mig_drop_audit")),
                "expected reserved-name-prefix for the wamn_ entity name, got {issues:?}"
            )
        }
        other => panic!("expected InvalidCatalog(reserved-name-prefix), got {other:?}"),
    }

    // The same holds for a reserved INDEX name — the whole `wamn_` family is
    // reserved, so an index aside-target can never be authored either.
    let mut t = entity("t", "keeper", vec![text_field("v")]);
    t.indexes.push(Index {
        name: "wamn_mig_drop_ix".into(),
        fields: vec!["v".into()],
        unique: false,
    });
    let v1 = mini(
        1,
        vec![entity("x", "audit", vec![text_field("v")]), t.clone()],
    );
    let v2 = mini(2, vec![entity("e", "audit", vec![text_field("v")]), t]);
    match Migration::migrate(&v1, &v2) {
        Err(CompileError::InvalidCatalog(issues)) => assert!(
            issues
                .iter()
                .any(|i| i.code == "reserved-name-prefix"
                    && i.message.contains("wamn_mig_drop_ix")),
            "expected reserved-name-prefix for the wamn_ index name, got {issues:?}"
        ),
        other => panic!("expected InvalidCatalog for the index name, got {other:?}"),
    }
}

#[test]
fn empty_diff_is_empty_plan() {
    let c = poc();
    let plan = Migration::migrate(&c, &c).expect("compiles");
    assert!(plan.is_empty());
    assert!(plan.is_additive());
    assert_eq!(plan.report(), "no changes\n");
}

#[test]
fn reserved_column_is_rejected() {
    let mut c = poc();
    c.entities[1].fields.push(text_field("tenant_id"));
    match Migration::create(&c) {
        Err(CompileError::ReservedColumn { field, .. }) => assert_eq!(field, "tenant_id"),
        other => panic!("expected ReservedColumn, got {other:?}"),
    }
}

#[test]
fn invalid_catalog_is_rejected() {
    let mut c = poc();
    c.entities.push(c.entities[0].clone()); // duplicate entity id
    match Migration::create(&c) {
        Err(CompileError::InvalidCatalog(issues)) => {
            assert!(issues.iter().any(|i| i.code == "duplicate-entity-id"))
        }
        other => panic!("expected InvalidCatalog, got {other:?}"),
    }
}

#[test]
fn outbox_triggers_cover_every_table_and_are_additive() {
    let c = poc();
    let plan = Migration::outbox_triggers(&c, &OutboxOptions::default()).expect("compiles");
    // One shared function + one trigger per entity table.
    assert_eq!(plan.operations.len(), c.entities.len() + 1);
    assert!(plan.is_additive());
    let sql = plan.sql(Confirmation::None).expect("additive");

    // The shared function: event vocabulary = lower(TG_OP) pinned to the "C"
    // collation (a Turkish/Azeri database default would lowercase INSERT to
    // 'ınsert', fail the outbox CHECK, and abort the user write), tenant from
    // the ROW on both the NEW and OLD paths, payload via to_jsonb, default
    // schema qualified.
    assert!(sql.contains("CREATE OR REPLACE FUNCTION wamn_outbox_event()"));
    assert!(
        sql.contains("INSERT INTO \"wamn_run\".\"outbox\" (tenant_id, table_name, event, payload)")
    );
    assert!(sql.contains("lower(TG_OP COLLATE \"C\")"));
    let f = &plan.operations[0];
    assert!(
        !f.sql.contains("current_setting"),
        "tenant comes from the row, not the claim (superuser seeds carry no claim)"
    );
    assert_eq!(f.entity, "", "function op is catalog-scoped");
    // The runtime precondition (the outbox must exist) and the target schema
    // are surfaced on the review surface — a mis-targeted or schema-drifted
    // apply must not read as a clean no-caveat plan.
    assert!(f.summary.contains("\"wamn_run\".\"outbox\""));
    assert!(f.note.as_deref().unwrap_or("").contains("fails at runtime"));

    // Branch-to-payload binding: the DELETE branch carries OLD, the
    // insert/update fall-through carries NEW — a swapped mutant must not pass.
    let body = &f.sql;
    let delete_branch = body
        .find("IF TG_OP = 'DELETE' THEN")
        .expect("delete branch");
    let end_if = body.find("END IF").expect("end of delete branch");
    let old_pos = body.find("to_jsonb(OLD)").expect("OLD payload");
    let new_pos = body.find("to_jsonb(NEW)").expect("NEW payload");
    assert!(
        delete_branch < old_pos && old_pos < end_if,
        "DELETE branch must carry OLD"
    );
    assert!(
        end_if < new_pos,
        "insert/update fall-through must carry NEW"
    );
    let old_tenant = body.find("OLD.tenant_id").expect("OLD tenant");
    let new_tenant = body.find("NEW.tenant_id").expect("NEW tenant");
    assert!(delete_branch < old_tenant && old_tenant < end_if);
    assert!(end_if < new_tenant);

    // One CONSTANT-named trigger per table: per-table trigger namespace makes
    // the name collision-free, and CREATE OR REPLACE + the constant name keep
    // re-apply idempotent and table renames from stacking a second trigger.
    assert!(sql.contains(
        "CREATE OR REPLACE TRIGGER wamn_outbox_event\n    \
         AFTER INSERT OR UPDATE OR DELETE ON \"receipts\"\n    \
         FOR EACH ROW EXECUTE FUNCTION wamn_outbox_event()"
    ));
    let trig = plan
        .operations
        .iter()
        .find(|o| o.sql.contains("ON \"receipts\""))
        .expect("receipts trigger op");
    assert_eq!(trig.entity, "receipts");
}

#[test]
fn outbox_schema_is_configurable_and_validated() {
    let c = poc();
    let plan = Migration::outbox_triggers(
        &c,
        &OutboxOptions {
            schema: "wamn_dispatch_demo".into(),
        },
    )
    .expect("compiles");
    assert!(
        plan.operations[0]
            .sql
            .contains("INSERT INTO \"wamn_dispatch_demo\".\"outbox\"")
    );

    // The schema is embedded inside the function body's dollar-quoted block, so
    // anything beyond a bare identifier is refused (quoting cannot protect
    // against a value containing the dollar tag).
    for bad in ["", "bad-name", "1st", "wamn.run", "a$wamn_outbox$b", "x y"] {
        match Migration::outbox_triggers(
            &c,
            &OutboxOptions {
                schema: bad.to_string(),
            },
        ) {
            Err(CompileError::InvalidOutboxSchema { schema }) => assert_eq!(schema, bad),
            other => panic!("expected InvalidOutboxSchema for {bad:?}, got {other:?}"),
        }
    }

    // The catalog is still validated on this entry point.
    let mut dup = c.clone();
    dup.entities.push(dup.entities[0].clone());
    assert!(matches!(
        Migration::outbox_triggers(&dup, &OutboxOptions::default()),
        Err(CompileError::InvalidCatalog(_))
    ));
}

#[test]
fn drop_outbox_triggers_is_gated_destructive() {
    let c = poc();
    let plan = Migration::drop_outbox_triggers(&c).expect("compiles");
    assert_eq!(plan.operations.len(), c.entities.len() + 1);
    assert!(plan.requires_confirmation());

    let err = plan.sql(Confirmation::None).unwrap_err();
    assert!(
        err.destructive
            .iter()
            .any(|s| s.contains("stop emitting row events"))
    );

    let sql = plan
        .sql(Confirmation::ConfirmedWithBackup)
        .expect("confirmed");
    assert!(sql.contains("DROP TRIGGER IF EXISTS wamn_outbox_event ON \"receipts\""));
    assert!(sql.contains("DROP FUNCTION IF EXISTS wamn_outbox_event()"));
    // Triggers drop before the function they reference.
    assert!(sql.find("DROP TRIGGER").unwrap() < sql.find("DROP FUNCTION").unwrap());
}

/// Drift guard: the columns and event vocabulary the emitted trigger writes
/// must exist in the production outbox (deploy/run-queue.sql) — and the
/// default [`OutboxOptions`] schema must be the one the deploy file creates.
#[test]
fn outbox_trigger_shape_matches_run_queue_deploy_file() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/run-queue.sql");
    let ddl = std::fs::read_to_string(path).expect("read deploy/run-queue.sql");

    let start = ddl
        .find("CREATE TABLE wamn_run.outbox")
        .expect("outbox table in deploy file");
    let block = &ddl[start..start + ddl[start..].find(");").expect("outbox block ends") + 2];
    for col in ["tenant_id", "table_name", "event", "payload"] {
        assert!(block.contains(col), "outbox column {col} missing:\n{block}");
    }
    assert!(
        block.contains("event IN ('insert', 'update', 'delete')"),
        "event CHECK literals must match lower(TG_OP)"
    );
}

/// Live verification: apply the emitted DDL to a throwaway Postgres. Gated on
/// `WAMN_DDL_PG_URL` (a superuser URL — the harness provisions the wamn_app role
/// and an ephemeral schema). Skips cleanly when unset.
#[test]
fn emitted_sql_applies_on_postgres() {
    let Ok(url) = std::env::var("WAMN_DDL_PG_URL") else {
        eprintln!("skipping emitted_sql_applies_on_postgres (set WAMN_DDL_PG_URL to run)");
        return;
    };

    let v1 = poc();
    let create = Migration::create(&v1).unwrap();

    // Additive evolution: add a nullable column + index.
    let mut v2 = v1.clone();
    v2.version = 2;
    let materials = v2
        .entities
        .iter_mut()
        .find(|e| e.id == "materials")
        .unwrap();
    materials.fields.push(text_field("grade"));
    let add = Migration::migrate(&v1, &v2).unwrap();

    // Destructive evolution: drop a column (confirmed + backup).
    let mut v3 = v2.clone();
    v3.version = 3;
    let suppliers = v3
        .entities
        .iter_mut()
        .find(|e| e.id == "suppliers")
        .unwrap();
    suppliers.fields.retain(|f| f.id != "contact_email");
    let drop = Migration::migrate(&v2, &v3).unwrap();

    let mut script = String::new();
    // Provision role + isolate in a fresh schema, then apply the three plans.
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         BEGIN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_ddl_test CASCADE;\n\
         CREATE SCHEMA wamn_ddl_test AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_ddl_test TO wamn_app;\n\
         SET search_path TO wamn_ddl_test;\n",
    );
    script.push_str(&create.sql(Confirmation::None).unwrap());
    script.push_str(&add.sql(Confirmation::None).unwrap());
    script.push_str(&drop.sql(Confirmation::ConfirmedWithBackup).unwrap());
    script.push_str("DROP SCHEMA wamn_ddl_test CASCADE;\n");

    let mut child = Command::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Live verification of the name-reuse ordering fix on a throwaway Postgres
/// (gated on `WAMN_DDL_PG_URL`, skips cleanly when unset): one migration that
/// renames a table into a reused name (with other changes on the same entity),
/// removes-and-re-adds a table under the same name (same index names, an
/// inbound FK with live data), and then a second migration redefining a
/// constraint and an index while keeping their names. Every one of these
/// failed to apply (42P07 / 42P01 / 42710) before the preamble ordering.
#[test]
fn migration_with_name_reuse_applies_on_postgres() {
    let Ok(url) = std::env::var("WAMN_DDL_PG_URL") else {
        eprintln!(
            "skipping migration_with_name_reuse_applies_on_postgres (set WAMN_DDL_PG_URL to run)"
        );
        return;
    };

    let audit_shape = |eid: &str| {
        let mut e = entity(eid, "audit", vec![text_field("val"), text_field("extra")]);
        e.indexes.push(Index {
            name: "audit_by_val".into(),
            fields: vec!["val".into()],
            unique: false,
        });
        e.constraints.push(Constraint::Unique {
            name: "audit_val_uniq".into(),
            fields: vec!["val".into()],
        });
        e
    };

    // v1: receipts + audit (indexes/unique) + log with a reference -> audit.
    let mut log1 = entity("y", "log", vec![text_field("msg")]);
    log1.fields.push(reference_field("audit_ref", "x"));
    let v1 = mini(
        1,
        vec![
            entity("b", "receipts", vec![text_field("val")]),
            audit_shape("x"),
            log1,
        ],
    );

    // v2: receipts renamed aside AND extended; a NEW receipts; audit removed
    // and re-added (new entity id, same table/index/constraint names); log's
    // reference removed (the inbound-FK unwind).
    let v2 = mini(
        2,
        vec![
            entity(
                "b",
                "receipts_old",
                vec![text_field("val"), text_field("note")],
            ),
            entity("d", "receipts", vec![text_field("other")]),
            audit_shape("e"),
            entity("y", "log", vec![text_field("msg")]),
        ],
    );

    // v3: redefine the recreated audit's index and unique constraint KEEPING
    // their names (drop-before-add ordering).
    let mut audit3 = entity("e", "audit", vec![text_field("val"), text_field("extra")]);
    audit3.indexes.push(Index {
        name: "audit_by_val".into(),
        fields: vec!["val".into(), "extra".into()],
        unique: false,
    });
    audit3.constraints.push(Constraint::Unique {
        name: "audit_val_uniq".into(),
        fields: vec!["val".into(), "extra".into()],
    });
    let v3 = mini(
        3,
        vec![
            entity(
                "b",
                "receipts_old",
                vec![text_field("val"), text_field("note")],
            ),
            entity("d", "receipts", vec![text_field("other")]),
            audit3,
            entity("y", "log", vec![text_field("msg")]),
        ],
    );

    // v4: rename audit -> audit_log WHILE redefining its unique constraint
    // under the kept name — the hoisted drop must follow the rename (42P01
    // otherwise).
    let mut audit4 = entity(
        "e",
        "audit_log",
        vec![text_field("val"), text_field("extra")],
    );
    audit4.indexes.push(Index {
        name: "audit_by_val".into(),
        fields: vec!["val".into(), "extra".into()],
        unique: false,
    });
    audit4.constraints.push(Constraint::Unique {
        name: "audit_val_uniq".into(),
        fields: vec!["val".into()],
    });
    let v4 = mini(
        4,
        vec![
            entity(
                "b",
                "receipts_old",
                vec![text_field("val"), text_field("note")],
            ),
            entity("d", "receipts", vec![text_field("other")]),
            audit4,
            entity("y", "log", vec![text_field("msg")]),
        ],
    );

    // v5: column-namespace reuse on the renamed table — drop field id 'val'
    // and re-add the same column NAME with a different type (the index and
    // unique constraint must re-key onto the new field id, which also
    // exercises the implicit-drop force-hoist).
    let mut audit5 = entity("e", "audit_log", vec![text_field("extra")]);
    audit5.fields.push(Field {
        id: "val2".into(),
        name: "val".into(),
        field_type: FieldType::Int,
        nullable: true,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    });
    audit5.indexes.push(Index {
        name: "audit_by_val".into(),
        fields: vec!["val2".into(), "extra".into()],
        unique: false,
    });
    audit5.constraints.push(Constraint::Unique {
        name: "audit_val_uniq".into(),
        fields: vec!["val2".into()],
    });
    let v5 = mini(
        5,
        vec![
            entity(
                "b",
                "receipts_old",
                vec![text_field("val"), text_field("note")],
            ),
            entity("d", "receipts", vec![text_field("other")]),
            audit5,
            entity("y", "log", vec![text_field("msg")]),
        ],
    );

    let create = Migration::create(&v1).unwrap();
    let reuse = Migration::migrate(&v1, &v2).unwrap();
    let redefine = Migration::migrate(&v2, &v3).unwrap();
    let rename_redefine = Migration::migrate(&v3, &v4).unwrap();
    let column_reuse = Migration::migrate(&v4, &v5).unwrap();

    let mut script = String::new();
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         BEGIN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_ddl_reuse_test CASCADE;\n\
         CREATE SCHEMA wamn_ddl_reuse_test AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_ddl_reuse_test TO wamn_app;\n\
         SET search_path TO wamn_ddl_reuse_test;\n",
    );
    script.push_str(&create.sql(Confirmation::None).unwrap());
    // Live data: the audit row is referenced by log (a real inbound FK), and
    // receipts carries a row that must survive its rename.
    script.push_str(
        "INSERT INTO receipts (tenant_id, val) VALUES ('t1', 'keep-me');\n\
         INSERT INTO audit (tenant_id, val, extra) VALUES ('t1', 'old-audit', 'x');\n\
         INSERT INTO log (tenant_id, msg, audit_ref) SELECT 't1', 'ref', id FROM audit;\n",
    );
    // The heart of the gate: this apply hit 42P07 before the fix.
    script.push_str(&reuse.sql(Confirmation::ConfirmedWithBackup).unwrap());
    script.push_str(
        "DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM receipts_old WHERE val = 'keep-me') = 1, 'renamed table kept its data';\n\
             ASSERT (SELECT count(*) FROM information_schema.columns WHERE table_schema = 'wamn_ddl_reuse_test' AND table_name = 'receipts_old' AND column_name = 'note') = 1, 'rename + add-column on one entity applied';\n\
             ASSERT (SELECT count(*) FROM receipts) = 0, 'reclaimed receipts is the fresh table';\n\
             ASSERT (SELECT count(*) FROM audit) = 0, 'reclaimed audit is the fresh table';\n\
             ASSERT (SELECT count(*) FROM pg_indexes WHERE schemaname = 'wamn_ddl_reuse_test' AND tablename = 'audit' AND indexname = 'audit_pkey') = 1, 'recreated audit owns the canonical pkey name';\n\
             ASSERT (SELECT count(*) FROM pg_indexes WHERE schemaname = 'wamn_ddl_reuse_test' AND tablename = 'audit' AND indexname = 'audit_by_val') = 1, 'recreated audit owns the reclaimed index name';\n\
             ASSERT (SELECT count(*) FROM pg_indexes WHERE schemaname = 'wamn_ddl_reuse_test' AND tablename = 'receipts_old' AND indexname = 'receipts_old_pkey') = 1, 'renamed table pkey followed the rename';\n\
             ASSERT (SELECT count(*) FROM pg_indexes WHERE schemaname = 'wamn_ddl_reuse_test' AND tablename = 'receipts' AND indexname = 'receipts_pkey') = 1, 'recreated receipts owns the canonical pkey name (no silent suffix drift)';\n\
             ASSERT to_regclass('wamn_ddl_reuse_test.wamn_mig_drop_audit') IS NULL, 'the renamed-aside table was dropped';\n\
             ASSERT (SELECT count(*) FROM information_schema.columns WHERE table_schema = 'wamn_ddl_reuse_test' AND table_name = 'log' AND column_name = 'audit_ref') = 0, 'inbound FK column unwound';\n\
         END $$;\n",
    );
    // Same-named constraint/index redefinition: hit 42710 / 42P07 before.
    script.push_str(&redefine.sql(Confirmation::ConfirmedWithBackup).unwrap());
    script.push_str(
        "DO $$ BEGIN\n\
             ASSERT (SELECT indexdef FROM pg_indexes WHERE schemaname = 'wamn_ddl_reuse_test' AND indexname = 'audit_by_val') LIKE '%extra%', 'index redefined in place under its name';\n\
             ASSERT (SELECT count(*) FROM pg_constraint WHERE conname = 'audit_val_uniq') = 1, 'unique constraint redefined under its name';\n\
         END $$;\n",
    );
    // Rename + same-named constraint redefinition in ONE bump: the hoisted
    // drop references the PRE-rename table name and precedes the rename.
    script.push_str(
        &rename_redefine
            .sql(Confirmation::ConfirmedWithBackup)
            .unwrap(),
    );
    script.push_str(
        "DO $$ BEGIN\n\
             ASSERT to_regclass('wamn_ddl_reuse_test.audit_log') IS NOT NULL, 'audit renamed to audit_log';\n\
             ASSERT (SELECT count(*) FROM pg_constraint WHERE conname = 'audit_val_uniq' AND conrelid = 'wamn_ddl_reuse_test.audit_log'::regclass) = 1, 'constraint redefined on the renamed table';\n\
         END $$;\n",
    );
    // Column-namespace reuse: drop field id 'val' and re-add the same column
    // name with a different type on audit_log (42701 before the fix).
    script.push_str(&column_reuse.sql(Confirmation::ConfirmedWithBackup).unwrap());
    script.push_str(
        "DO $$ BEGIN\n\
             ASSERT (SELECT data_type FROM information_schema.columns WHERE table_schema = 'wamn_ddl_reuse_test' AND table_name = 'audit_log' AND column_name = 'val') = 'integer', 'column redefined in place under its name';\n\
         END $$;\n\
         DROP SCHEMA wamn_ddl_reuse_test CASCADE;\n",
    );

    let mut child = Command::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Live verification of the removed-entity drop ordering (wamn-nqg) on a
/// throwaway Postgres (gated on `WAMN_DDL_PG_URL`, skips cleanly when unset):
/// a chain authors <- books <- reviews with live rows and real inbound FKs is
/// dropped whole in one migration. The `DROP TABLE`s must emit dependents-first
/// or Postgres fails 2BP01 (`constraint books_author_id_fkey on table books
/// depends on table authors`) and the one-txn apply rolls back — the exact bug
/// this closes. Runs in its OWN schema so it parallelizes with the other gates.
#[test]
fn removed_entity_drops_apply_on_postgres() {
    let Ok(url) = std::env::var("WAMN_DDL_PG_URL") else {
        eprintln!("skipping removed_entity_drops_apply_on_postgres (set WAMN_DDL_PG_URL to run)");
        return;
    };

    let authors = entity("e1_root", "authors", vec![text_field("name")]);
    let mut books = entity("e2_mid", "books", vec![text_field("title")]);
    books.fields.push(reference_field("author_id", "e1_root"));
    let mut reviews = entity("e3_leaf", "reviews", vec![text_field("body")]);
    reviews.fields.push(reference_field("book_id", "e2_mid"));
    let v1 = mini(1, vec![authors, books, reviews]);
    let v2 = mini(2, vec![]);

    let create = Migration::create(&v1).unwrap();
    let drop_all = Migration::migrate(&v1, &v2).unwrap();

    let mut script = String::new();
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         BEGIN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_ddl_drop_order_test CASCADE;\n\
         CREATE SCHEMA wamn_ddl_drop_order_test AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_ddl_drop_order_test TO wamn_app;\n\
         SET search_path TO wamn_ddl_drop_order_test;\n",
    );
    script.push_str(&create.sql(Confirmation::None).unwrap());
    // Live rows exercising both FK edges (reviews -> books -> authors), so a
    // wrong drop order fails on a real dependency, not just an empty catalog.
    script.push_str(
        "INSERT INTO authors (tenant_id, name) VALUES ('t1', 'Le Guin');\n\
         INSERT INTO books (tenant_id, title, author_id) SELECT 't1', 'Earthsea', id FROM authors;\n\
         INSERT INTO reviews (tenant_id, body, book_id) SELECT 't1', 'wizardly', id FROM books;\n",
    );
    // The heart of the gate: this apply hit 2BP01 before the topological order.
    script.push_str(&drop_all.sql(Confirmation::ConfirmedWithBackup).unwrap());
    script.push_str(
        "DO $$ BEGIN\n\
             ASSERT to_regclass('wamn_ddl_drop_order_test.authors') IS NULL, 'authors dropped';\n\
             ASSERT to_regclass('wamn_ddl_drop_order_test.books') IS NULL, 'books dropped';\n\
             ASSERT to_regclass('wamn_ddl_drop_order_test.reviews') IS NULL, 'reviews dropped';\n\
         END $$;\n\
         DROP SCHEMA wamn_ddl_drop_order_test CASCADE;\n",
    );

    let mut child = Command::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Live verification of the empty-tenant floor hardening (wamn-a45) on a
/// throwaway Postgres (gated on `WAMN_DDL_PG_URL`, skips cleanly when unset).
/// Postgres resets a custom GUC to `''` (not NULL) once `SET LOCAL` scope ends,
/// so an idle claimless pooled connection carries `app.tenant = ''`. This gate
/// proves BOTH halves of the structural fix independently, in its own schema so
/// it parallelizes with the other gates:
///   (a) `CHECK (tenant_id <> '')` forbids a `''`-tenant row even for a
///       superuser / BYPASSRLS writer (the exact attack path);
///   (b) the policy's `NULLIF(current_setting('app.tenant', true), '')` hides a
///       `''`-tenant row from an empty claim — proven after the CHECK is dropped
///       so a `''`-row can actually be planted, isolating the policy read.
#[test]
fn empty_tenant_claim_matches_no_row_on_postgres() {
    let Ok(url) = std::env::var("WAMN_DDL_PG_URL") else {
        eprintln!(
            "skipping empty_tenant_claim_matches_no_row_on_postgres (set WAMN_DDL_PG_URL to run)"
        );
        return;
    };

    let notes = entity("en", "notes", vec![text_field("body")]);
    let v1 = mini(1, vec![notes]);
    let create = Migration::create(&v1).unwrap();

    let mut script = String::new();
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         BEGIN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_ddl_empty_tenant_test CASCADE;\n\
         CREATE SCHEMA wamn_ddl_empty_tenant_test AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_ddl_empty_tenant_test TO wamn_app;\n\
         SET search_path TO wamn_ddl_empty_tenant_test;\n",
    );
    script.push_str(&create.sql(Confirmation::None).unwrap());
    // Seed one legitimate row (as superuser — BYPASSRLS; the CHECK still applies
    // and 't1' <> '' passes).
    script.push_str("INSERT INTO notes (tenant_id, body) VALUES ('t1', 'hello');\n");
    // (a) The CHECK rejects a ''-tenant row even for a superuser writer.
    script.push_str(
        "DO $$ BEGIN\n\
             BEGIN\n\
                 INSERT INTO notes (tenant_id, body) VALUES ('', 'ghost');\n\
                 RAISE EXCEPTION 'expected a check_violation but the empty-tenant insert succeeded';\n\
             EXCEPTION WHEN check_violation THEN NULL;\n\
             END;\n\
         END $$;\n",
    );
    // An empty or unset claim (the reset-GUC value) sees no rows; a real claim
    // sees its own — proving RLS is active and the table is not simply empty.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL app.tenant = '';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM notes) = 0, 'empty claim sees no rows'; END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM notes) = 0, 'unset claim sees no rows'; END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL app.tenant = 't1';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM notes) = 1, 'legit claim sees its row'; END $$;\n\
         COMMIT;\n",
    );
    // (b) Isolate the policy's NULLIF: drop the belt-and-braces CHECK (proven
    // above), plant a ''-tenant row, and confirm an empty claim STILL sees
    // nothing. Bare current_setting would match the ghost here; NULLIF => NULL
    // => no match. This is what fails if rls_op loses its NULLIF.
    script.push_str(
        "ALTER TABLE notes DROP CONSTRAINT notes_tenant_id_check;\n\
         INSERT INTO notes (tenant_id, body) VALUES ('', 'ghost');\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL app.tenant = '';\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM notes) = 0, 'empty claim hides a ''-tenant row (NULLIF, not just the CHECK)';\n\
         END $$;\n\
         COMMIT;\n\
         DROP SCHEMA wamn_ddl_empty_tenant_test CASCADE;\n",
    );

    let mut child = Command::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Live verification of the special-value floor CHECKs (wamn-oj7) on a throwaway
/// Postgres (gated on `WAMN_DDL_PG_URL`, skips cleanly when unset). In its own
/// schema so it parallelizes with the other gates. Proves that on a generated
/// table a normal decimal / date / timestamp inserts fine, but `'NaN'::numeric`,
/// `'infinity'::timestamptz`, and `'infinity'::date` are each rejected at the DB
/// — the flow-authored-SQL path (the only way `NaN` reaches the column, since
/// the gateway already blocks it) is blocked by the floor.
#[test]
fn special_values_are_rejected_on_postgres() {
    let Ok(url) = std::env::var("WAMN_DDL_PG_URL") else {
        eprintln!("skipping special_values_are_rejected_on_postgres (set WAMN_DDL_PG_URL to run)");
        return;
    };

    let readings = entity(
        "er",
        "readings",
        vec![
            field_of(
                "qty",
                FieldType::Numeric {
                    precision: 10,
                    scale: 2,
                    unit: None,
                },
            ),
            field_of("d", FieldType::Date),
            field_of("ts", FieldType::Timestamptz),
        ],
    );
    let create = Migration::create(&mini(1, vec![readings])).unwrap();

    let mut script = String::new();
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         BEGIN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_ddl_special_values_test CASCADE;\n\
         CREATE SCHEMA wamn_ddl_special_values_test AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_ddl_special_values_test TO wamn_app;\n\
         SET search_path TO wamn_ddl_special_values_test;\n",
    );
    script.push_str(&create.sql(Confirmation::None).unwrap());
    // A normal decimal / date / timestamp inserts fine.
    script.push_str(
        "INSERT INTO readings (tenant_id, qty, d, ts) \
         VALUES ('t1', '12.50', '2026-07-13', '2026-07-13T00:00:00Z');\n",
    );
    // Each special value is rejected by its CHECK (a check_violation).
    for (col, val, label) in [
        ("qty", "NaN", "NaN numeric"),
        ("ts", "infinity", "infinity timestamptz"),
        ("d", "infinity", "infinity date"),
    ] {
        script.push_str(&format!(
            "DO $$ BEGIN\n\
                 BEGIN\n\
                     INSERT INTO readings (tenant_id, {col}) VALUES ('t1', '{val}');\n\
                     RAISE EXCEPTION 'expected a check_violation but the {label} insert succeeded';\n\
                 EXCEPTION WHEN check_violation THEN NULL;\n\
                 END;\n\
             END $$;\n",
        ));
    }
    script.push_str("DROP SCHEMA wamn_ddl_special_values_test CASCADE;\n");

    let mut child = Command::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Live verification of the reference-retype FK lifecycle (wamn-drb) on a
/// throwaway Postgres (gated on `WAMN_DDL_PG_URL`, skips cleanly when unset).
/// Two scenarios in one dedicated schema so it parallelizes with the other
/// gates: (A) a Reference retyped to Uuid while its target table is removed —
/// the synthesized FK must drop or the `DROP TABLE` fails 2BP01, and the
/// referencing row must survive; (B) a Uuid retyped into a Reference — the
/// added FK must be a real enforcing constraint (a bogus insert is rejected).
#[test]
fn reference_retype_fk_lifecycle_applies_on_postgres() {
    let Ok(url) = std::env::var("WAMN_DDL_PG_URL") else {
        eprintln!(
            "skipping reference_retype_fk_lifecycle_applies_on_postgres (set WAMN_DDL_PG_URL to run)"
        );
        return;
    };

    // Scenario A: audit <- log(aref); remove audit AND retype aref -> uuid.
    let audit = entity("ea", "audit", vec![text_field("val")]);
    let mut log_ref = entity("el", "log", vec![text_field("msg")]);
    log_ref.fields.push(reference_field("aref", "ea"));
    let a_v1 = mini(1, vec![audit, log_ref]);
    let mut log_uuid = entity("el", "log", vec![text_field("msg")]);
    log_uuid.fields.push(uuid_field("aref"));
    let a_v2 = mini(2, vec![log_uuid]);
    let create_a = Migration::create(&a_v1).unwrap();
    let retype_drop = Migration::migrate(&a_v1, &a_v2).unwrap();

    // Scenario B: people + tag(pid uuid); retype pid -> Reference{people}.
    let people = entity("ep", "people", vec![text_field("name")]);
    let mut tag_uuid = entity("et", "tag", vec![text_field("label")]);
    tag_uuid.fields.push(uuid_field("pid"));
    let b_v1 = mini(1, vec![people.clone(), tag_uuid]);
    let mut tag_ref = entity("et", "tag", vec![text_field("label")]);
    tag_ref.fields.push(reference_field("pid", "ep"));
    let b_v2 = mini(2, vec![people, tag_ref]);
    let create_b = Migration::create(&b_v1).unwrap();
    let retype_add = Migration::migrate(&b_v1, &b_v2).unwrap();

    let mut script = String::new();
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         BEGIN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_ddl_retype_fk_test CASCADE;\n\
         CREATE SCHEMA wamn_ddl_retype_fk_test AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_ddl_retype_fk_test TO wamn_app;\n\
         SET search_path TO wamn_ddl_retype_fk_test;\n",
    );
    // Scenario A: seed a real inbound FK row, then the retype-and-drop apply.
    script.push_str(&create_a.sql(Confirmation::None).unwrap());
    script.push_str(
        "INSERT INTO audit (tenant_id, val) VALUES ('t1', 'keep');\n\
         INSERT INTO log (tenant_id, msg, aref) SELECT 't1', 'r', id FROM audit;\n",
    );
    // Hit 2BP01 before the fix — the FK survived the retype and blocked the drop.
    script.push_str(&retype_drop.sql(Confirmation::ConfirmedWithBackup).unwrap());
    script.push_str(
        "DO $$ BEGIN\n\
             ASSERT to_regclass('wamn_ddl_retype_fk_test.audit') IS NULL, 'audit dropped';\n\
             ASSERT (SELECT count(*) FROM log WHERE aref IS NOT NULL) = 1, 'referencing row survived the retype';\n\
             ASSERT (SELECT count(*) FROM pg_constraint WHERE conname = 'log_aref_fkey') = 0, 'stale FK gone';\n\
         END $$;\n",
    );
    // Scenario B: seed a valid row, retype into a Reference, then prove the
    // added FK actually enforces.
    script.push_str(&create_b.sql(Confirmation::None).unwrap());
    script.push_str(
        "INSERT INTO people (tenant_id, name) VALUES ('t1', 'p');\n\
         INSERT INTO tag (tenant_id, pid) SELECT 't1', id FROM people;\n",
    );
    script.push_str(&retype_add.sql(Confirmation::ConfirmedWithBackup).unwrap());
    script.push_str(
        "DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM pg_constraint WHERE conname = 'tag_pid_fkey') = 1, 'FK added on entering Reference';\n\
             BEGIN\n\
                 INSERT INTO tag (tenant_id, pid) VALUES ('t1', gen_random_uuid());\n\
                 RAISE EXCEPTION 'expected a foreign_key_violation but the insert succeeded';\n\
             EXCEPTION WHEN foreign_key_violation THEN NULL;\n\
             END;\n\
         END $$;\n\
         DROP SCHEMA wamn_ddl_retype_fk_test CASCADE;\n",
    );

    let mut child = Command::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Live behavioral verification of the outbox triggers on a throwaway Postgres
/// (gated on `WAMN_DDL_PG_URL`, skips cleanly when unset): a `wamn_app` write
/// emits exactly one event row **in the same transaction** (D4) with the exact
/// event vocabulary, the exact-decimal payload preserved byte-for-byte (no
/// float round trip), tenant taken from the row (superuser seeds fire too),
/// outbox RLS isolating tenants, an `ON CONFLICT DO NOTHING` no-op emitting
/// nothing, a re-applied plan not stacking a duplicate trigger, and the drop
/// plan silencing emission.
#[test]
fn outbox_triggers_fire_on_postgres() {
    let Ok(url) = std::env::var("WAMN_DDL_PG_URL") else {
        eprintln!("skipping outbox_triggers_fire_on_postgres (set WAMN_DDL_PG_URL to run)");
        return;
    };

    // A minimal one-table catalog with an exact-decimal column.
    let catalog = Catalog {
        schema_version: "0.1".into(),
        catalog_id: "wp4-receipts".into(),
        version: 1,
        name: None,
        entities: vec![Entity {
            id: "receipts".into(),
            name: "receipts".into(),
            is_system: false,
            label: None,
            description: None,
            fields: vec![
                Field {
                    id: "qty".into(),
                    name: "qty".into(),
                    field_type: FieldType::Numeric {
                        precision: 10,
                        scale: 2,
                        unit: Some("kg".into()),
                    },
                    nullable: false,
                    default: None,
                    sensitive: false,
                    is_system: false,
                    label: None,
                    description: None,
                },
                text_field("note"),
            ],
            indexes: vec![],
            constraints: vec![],
        }],
        relations: vec![],
    };
    let floor = Migration::create(&catalog).unwrap();
    // The test outbox lives in the ephemeral schema itself — which also
    // exercises the schema-qualified reference the production 'wamn_run'
    // default relies on.
    let opts = OutboxOptions {
        schema: "wamn_ddl_outbox_test".into(),
    };
    let triggers = Migration::outbox_triggers(&catalog, &opts).unwrap();

    // Rename evolution: receipts -> receipts2 (same entity id). The trigger
    // follows the rename; re-applying the v2 outbox plan must REPLACE it (the
    // constant name), not stack a second one.
    let mut v2 = catalog.clone();
    v2.version = 2;
    v2.entities[0].name = "receipts2".into();
    let rename = Migration::migrate(&catalog, &v2).unwrap();
    let triggers_v2 = Migration::outbox_triggers(&v2, &opts).unwrap();
    let drop = Migration::drop_outbox_triggers(&v2).unwrap();

    const R1: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    let mut script = String::new();
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         BEGIN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_ddl_outbox_test CASCADE;\n\
         CREATE SCHEMA wamn_ddl_outbox_test AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_ddl_outbox_test TO wamn_app;\n\
         SET search_path TO wamn_ddl_outbox_test;\n",
    );
    // Inline replica of the production outbox (deploy/run-queue.sql,
    // drift-guarded by outbox_trigger_shape_matches_run_queue_deploy_file).
    script.push_str(
        "CREATE TABLE outbox (\n\
             tenant_id     text NOT NULL CHECK (tenant_id <> ''),\n\
             seq           bigint GENERATED ALWAYS AS IDENTITY,\n\
             table_name    text NOT NULL,\n\
             event         text NOT NULL CHECK (event IN ('insert', 'update', 'delete')),\n\
             payload       jsonb,\n\
             created_at    timestamptz NOT NULL DEFAULT now(),\n\
             dispatched_at timestamptz,\n\
             PRIMARY KEY (tenant_id, seq)\n\
         );\n\
         ALTER TABLE outbox ENABLE ROW LEVEL SECURITY;\n\
         ALTER TABLE outbox FORCE ROW LEVEL SECURITY;\n\
         CREATE POLICY outbox_tenant ON outbox\n\
             USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))\n\
             WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));\n\
         GRANT SELECT, INSERT, UPDATE, DELETE ON outbox TO wamn_app;\n",
    );
    script.push_str(&floor.sql(Confirmation::None).unwrap());
    script.push_str(&triggers.sql(Confirmation::None).unwrap());
    // Re-apply the whole trigger plan: CREATE OR REPLACE + the constant trigger
    // name must not stack a second trigger (asserted by the counts below).
    script.push_str(&triggers.sql(Confirmation::None).unwrap());

    // 1) A wamn_app INSERT emits exactly one 'insert' event in the SAME
    //    transaction (D4), with the exact-decimal payload preserved.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         INSERT INTO receipts (id, tenant_id, qty, note) VALUES ('{R1}', 't1', 12.50, 'first');\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox) = 1, 'exactly one event (no duplicate trigger from the re-applied plan)';\n\
             ASSERT (SELECT count(*) FROM outbox WHERE event = 'insert' AND table_name = 'receipts' AND tenant_id = 't1') = 1, 'insert event shape';\n\
             ASSERT (SELECT payload->>'qty' FROM outbox WHERE event = 'insert') = '12.50', 'exact-decimal preserved';\n\
             ASSERT (SELECT payload::text FROM outbox WHERE event = 'insert') LIKE '%12.50%', 'payload text not float-rounded';\n\
             ASSERT (SELECT payload->>'note' FROM outbox WHERE event = 'insert') = 'first', 'full row in payload';\n\
         END $$;\n\
         COMMIT;\n"
    ));
    // 2) UPDATE emits an 'update' event carrying NEW values.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         UPDATE receipts SET qty = 13.00 WHERE id = '{R1}';\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox WHERE event = 'update') = 1, 'one update event';\n\
             ASSERT (SELECT payload->>'qty' FROM outbox WHERE event = 'update') = '13.00', 'update carries NEW';\n\
         END $$;\n\
         COMMIT;\n"
    ));
    // 3) An ON CONFLICT DO NOTHING no-op (a 3.6 re-seed) emits nothing.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         INSERT INTO receipts (id, tenant_id, qty, note) VALUES ('{R1}', 't1', 99.99, 'dup') ON CONFLICT (id) DO NOTHING;\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox) = 2, 'no event from a conflict no-op';\n\
         END $$;\n\
         COMMIT;\n"
    ));
    // 4) DELETE emits a 'delete' event carrying OLD.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         DELETE FROM receipts WHERE id = '{R1}';\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox WHERE event = 'delete') = 1, 'one delete event';\n\
             ASSERT (SELECT payload->>'qty' FROM outbox WHERE event = 'delete') = '13.00', 'delete carries OLD';\n\
         END $$;\n\
         COMMIT;\n"
    ));
    // 5) A superuser seed (BYPASSRLS, no app.tenant claim) fires too — tenant
    //    comes from the ROW, not the claim.
    script.push_str(
        "INSERT INTO receipts (tenant_id, qty, note) VALUES ('t2', 1.00, 'seed');\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox WHERE tenant_id = 't2' AND event = 'insert') = 1, 'superuser seed fires with row tenant';\n\
         END $$;\n",
    );
    // 6) Outbox RLS isolates tenants: t1 sees only its own 3 events.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox) = 3, 't1 sees only t1 events';\n\
         END $$;\n\
         COMMIT;\n",
    );
    // 7) Rename-safety: apply the v2 rename migration, re-apply the v2 outbox
    //    plan, and prove a write to the renamed table fires EXACTLY ONCE — the
    //    renamed table kept its trigger, and the constant-named CREATE OR
    //    REPLACE replaced it rather than stacking a second.
    script.push_str(&rename.sql(Confirmation::ConfirmedWithBackup).unwrap());
    script.push_str(&triggers_v2.sql(Confirmation::None).unwrap());
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         INSERT INTO receipts2 (tenant_id, qty, note) VALUES ('t1', 7.25, 'renamed');\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox WHERE table_name = 'receipts2') = 1, 'exactly one event after rename + re-apply (no stacked trigger)';\n\
         END $$;\n\
         COMMIT;\n",
    );
    // 8) The (confirmed) drop plan silences emission.
    script.push_str(&drop.sql(Confirmation::ConfirmedWithBackup).unwrap());
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         INSERT INTO receipts2 (tenant_id, qty, note) VALUES ('t1', 5.00, 'after-drop');\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox WHERE tenant_id = 't1') = 4, 'no event after the drop plan';\n\
         END $$;\n\
         COMMIT;\n",
    );
    script.push_str("DROP SCHEMA wamn_ddl_outbox_test CASCADE;\n");

    let mut child = Command::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// --- expression-chaining guard (cjv.5 / review C1-1) -----------------------

/// A single-entity catalog whose only constraint is a `Check` carrying `expr`.
fn check_catalog(name: &str, expr: &str) -> Catalog {
    let mut e = entity(name, name, vec![text_field("code")]);
    e.fields.push(Field {
        id: "qty".into(),
        name: "qty".into(),
        field_type: FieldType::Int,
        nullable: false,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    });
    e.constraints.push(Constraint::Check {
        name: format!("{name}_ck"),
        expression: expr.into(),
    });
    mini(1, vec![e])
}

fn run_psql(url: &str, script: &str) -> std::process::Output {
    let mut child = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// `Migration::create`/`migrate` call `check()` → `catalog.validate()` first, so
/// a `Check` expression that could chain statements is rejected before any SQL
/// is emitted — it never reaches the exec path.
#[test]
fn check_constraint_with_a_chaining_expression_is_rejected_before_emission() {
    let cat = check_catalog("gizmo", "1=1); DROP TABLE app_system.users; --");
    match Migration::create(&cat) {
        Err(CompileError::InvalidCatalog(issues)) => assert!(
            issues.iter().any(|i| i.code == "unsafe-check-expression"),
            "expected unsafe-check-expression, got {issues:?}"
        ),
        other => panic!("expected InvalidCatalog(unsafe-check-expression), got {other:?}"),
    }
}

/// The guard passes a legitimate `Check` expression through to a working
/// `ADD CONSTRAINT … CHECK (…)` — it forbids statement chaining, not the
/// expression's logic.
#[test]
fn a_legitimate_check_constraint_still_compiles() {
    let plan =
        Migration::create(&check_catalog("gizmo", "qty >= 0")).expect("a safe Check compiles");
    let sql = plan.sql(Confirmation::None).unwrap();
    assert!(
        sql.contains("ADD CONSTRAINT \"gizmo_ck\" CHECK (qty >= 0)"),
        "emitted SQL missing the Check constraint:\n{sql}"
    );
}

/// Live proof (gated on `WAMN_DDL_PG_URL`): a legitimate `Check` applies, and a
/// chaining `Check` is rejected at compile time so its `DROP` never reaches
/// Postgres. Were the guard neutered, `Migration::create` would return `Ok`, the
/// emitted plan would chain the `DROP`, and this test applies it to demonstrate
/// the danger and fail loudly. Skips cleanly when the env var is unset.
#[test]
fn chaining_check_expression_never_reaches_postgres() {
    let Ok(url) = std::env::var("WAMN_DDL_PG_URL") else {
        eprintln!(
            "skipping chaining_check_expression_never_reaches_postgres (set WAMN_DDL_PG_URL to run)"
        );
        return;
    };
    const SCHEMA: &str = "wamn_ddl_expr_guard_test";

    // Provision role + a fresh schema + the sentinel table the exploit targets,
    // then apply a legitimate Check (proving the guard does not over-reject).
    let mut setup = String::from(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         BEGIN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; END IF; END $$;\n",
    );
    setup.push_str(&format!(
        "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;\n\
         CREATE SCHEMA {SCHEMA} AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_app;\n\
         SET search_path TO {SCHEMA};\n\
         CREATE TABLE guard_sentinel (id int);\n\
         INSERT INTO guard_sentinel VALUES (1);\n"
    ));
    let safe = Migration::create(&check_catalog("part", "qty >= 0")).expect("safe Check compiles");
    setup.push_str(&safe.sql(Confirmation::None).unwrap());
    let out = run_psql(&url, &setup);
    assert!(
        out.status.success(),
        "setup psql failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The chaining Check is rejected before emission; a neutered guard is caught
    // by applying the emitted plan (which chains a DROP) and failing.
    let malicious = check_catalog("gizmo", "1=1); DROP TABLE guard_sentinel; --");
    match Migration::create(&malicious) {
        Err(CompileError::InvalidCatalog(issues)) => assert!(
            issues.iter().any(|i| i.code == "unsafe-check-expression"),
            "expected unsafe-check-expression, got {issues:?}"
        ),
        Ok(plan) => {
            let sql = plan.sql(Confirmation::None).unwrap();
            let _ = run_psql(&url, &format!("SET search_path TO {SCHEMA};\n{sql}"));
            panic!(
                "guard did not reject the chaining Check; the emitted plan chains a DROP:\n{sql}"
            );
        }
        other => panic!("unexpected compile result: {other:?}"),
    }

    // The sentinel is untouched: the DROP never reached Postgres. (If it had, the
    // SELECT would error under ON_ERROR_STOP.)
    let verify = format!(
        "SET search_path TO {SCHEMA};\n\
         SELECT 'SENTINEL_COUNT=' || count(*) FROM guard_sentinel;\n\
         DROP SCHEMA {SCHEMA} CASCADE;\n"
    );
    let out = run_psql(&url, &verify);
    assert!(
        out.status.success(),
        "verify psql failed (the sentinel may have been dropped):\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("SENTINEL_COUNT=1"),
        "sentinel row missing — the chained DROP must have run:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}
