//! The column cutover that renames `kind` columns to `type`
//! (docs/plan/kind-to-type.md §4.3.2).
//!
//! It renames P8 (`catalog.package_definition_owners`) and P10 to P12 (the
//! run-plane tables) of an installed database. Each table is in one of three
//! states: every old column and no new one (renamed), no old column (skipped),
//! or anything else (refused, because that state has no safe rename).

use std::collections::{BTreeMap, BTreeSet};

use super::{BareSchemaName, RunPlaneObservation, rewrite_schema};

/// One run-plane table of the cutover: its renamed columns and checks.
struct TableRename {
    table: &'static str,
    columns: &'static [(&'static str, &'static str)],
    checks: &'static [(&'static str, &'static str)],
}

const RUN_PLANE_RENAMES: [TableRename; 3] = [
    TableRename {
        table: "runs",
        columns: &[
            ("caller_outcome_kind", "caller_outcome_type"),
            ("fail_kind", "fail_type"),
        ],
        checks: &[
            (
                "runs_caller_outcome_kind_check",
                "runs_caller_outcome_type_check",
            ),
            ("runs_fail_kind_check", "runs_fail_type_check"),
        ],
    },
    TableRename {
        table: "effect_attempts",
        columns: &[("generation_fact_kind", "generation_fact_type")],
        checks: &[],
    },
    TableRename {
        table: "operator_run_actions",
        columns: &[
            ("action_kind", "action_type"),
            ("principal_kind", "principal_type"),
        ],
        checks: &[
            (
                "operator_run_actions_kind_check",
                "operator_run_actions_type_check",
            ),
            (
                "operator_run_actions_principal_kind_check",
                "operator_run_actions_principal_type_check",
            ),
        ],
    },
];

/// The `NOT NULL` names of P8, P11 and P12. The verb does not observe them, so
/// the batch renames each one only when it exists.
const NOT_NULL_BLOCK: &str = r"DO $type_column_not_null$
DECLARE
    renamed record;
BEGIN
    FOR renamed IN
        SELECT * FROM (VALUES
            ('catalog.package_definition_owners', 'package_definition_owners_definition_kind_not_null', 'package_definition_owners_definition_type_not_null'),
            ('wamn_run.effect_attempts', 'effect_attempts_generation_fact_kind_not_null', 'effect_attempts_generation_fact_type_not_null'),
            ('wamn_run.operator_run_actions', 'operator_run_actions_action_kind_not_null', 'operator_run_actions_action_type_not_null'),
            ('wamn_run.operator_run_actions', 'operator_run_actions_principal_kind_not_null', 'operator_run_actions_principal_type_not_null')
        ) AS names (relation_name, old_name, new_name)
    LOOP
        IF EXISTS (SELECT FROM pg_catalog.pg_constraint
                    WHERE conrelid = to_regclass(renamed.relation_name)
                      AND conname = renamed.old_name) THEN
            EXECUTE format('ALTER TABLE %s RENAME CONSTRAINT %I TO %I',
                           to_regclass(renamed.relation_name), renamed.old_name, renamed.new_name);
        END IF;
    END LOOP;
END
$type_column_not_null$;";

/// The P8 part. The verb does not observe the checks of `catalog`, so the batch
/// guards the check and rolls back when it is missing.
const DEFINITION_OWNERS_PART: &str = r"ALTER TABLE catalog.package_definition_owners RENAME COLUMN definition_kind TO definition_type;
DO $type_column_p8_check$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_constraint
                    WHERE conrelid = 'catalog.package_definition_owners'::regclass
                      AND conname = 'package_definition_owners_definition_kind_check') THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'type-column-cutover: package_definition_owners_definition_kind_check is missing';
    END IF;
    ALTER TABLE catalog.package_definition_owners
        RENAME CONSTRAINT package_definition_owners_definition_kind_check
        TO package_definition_owners_definition_type_check;
END
$type_column_p8_check$;";

/// What the cutover does to one observed database.
pub(super) enum TypeColumnCutover {
    /// Every table is already renamed, or absent.
    None,
    /// A table is half renamed, or an old check is missing. The SQL raises
    /// SQLSTATE 55000 and nothing else runs.
    Refuse(String),
    /// Rename the tables in the old state. `renamed` is the observation with
    /// the new names applied, so the rest of the plan sees the renamed tables.
    Rename {
        sql: String,
        renamed: Box<RunPlaneObservation>,
    },
}

enum State {
    Old,
    New,
    Mixed,
}

fn state(columns: &BTreeSet<String>, renames: &[(&str, &str)]) -> State {
    let old = renames
        .iter()
        .filter(|(old, _)| columns.contains(*old))
        .count();
    let new = renames
        .iter()
        .filter(|(_, new)| columns.contains(*new))
        .count();
    if old == renames.len() && new == 0 {
        State::Old
    } else if old == 0 {
        // Every new column, or a table from before these columns existed:
        // nothing is renamed, and the rest of the plan adds what is missing.
        State::New
    } else {
        State::Mixed
    }
}

fn refusal(reason: &str) -> TypeColumnCutover {
    TypeColumnCutover::Refuse(format!(
        "DO $type_column_cutover$ BEGIN RAISE EXCEPTION USING ERRCODE = '55000', \
         MESSAGE = 'type-column-cutover: {reason}'; END $type_column_cutover$;"
    ))
}

/// Replace each old column name in `text` with its new name, whole words only.
fn rename_words(text: &str, renames: &[(&str, &str)]) -> String {
    renames.iter().fold(text.to_owned(), |text, (old, new)| {
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(at) = rest.find(old) {
            let before = rest[..at].chars().next_back();
            let after = rest[at + old.len()..].chars().next();
            let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            out.push_str(&rest[..at]);
            out.push_str(if word(before) || word(after) {
                old
            } else {
                new
            });
            rest = &rest[at + old.len()..];
        }
        out.push_str(rest);
        out
    })
}

fn rename_keys<V: Clone>(
    map: &BTreeMap<(String, String), V>,
    table: &str,
    renames: &[(&str, &str)],
) -> BTreeMap<(String, String), V> {
    map.iter()
        .map(|((owner, name), value)| {
            let name = if owner == table {
                renames
                    .iter()
                    .find(|(old, _)| old == name)
                    .map_or_else(|| name.clone(), |(_, new)| (*new).to_owned())
            } else {
                name.clone()
            };
            ((owner.clone(), name), value.clone())
        })
        .collect()
}

/// Plan the cutover for one observed database.
pub(super) fn type_column_cutover(
    schema: &BareSchemaName,
    obs: &RunPlaneObservation,
) -> TypeColumnCutover {
    let mut parts = Vec::new();
    let mut locked = Vec::new();
    let mut renamed = obs.clone();

    match (
        obs.definition_owners_kind_column,
        obs.definition_owners_type_column,
    ) {
        (true, false) => {
            locked.push("catalog.package_definition_owners".to_owned());
            parts.push(DEFINITION_OWNERS_PART.to_owned());
            renamed.definition_owners_kind_column = false;
            renamed.definition_owners_type_column = true;
        }
        (true, true) => return refusal("package_definition_owners is half renamed"),
        _ => {}
    }

    for rename in &RUN_PLANE_RENAMES {
        let Some(columns) = obs.tables.get(rename.table) else {
            continue;
        };
        match state(columns, rename.columns) {
            State::New => continue,
            State::Mixed => return refusal(&format!("{} is half renamed", rename.table)),
            State::Old => {}
        }
        for (old, _) in rename.checks {
            if !obs
                .checks
                .contains_key(&(rename.table.to_owned(), (*old).to_owned()))
            {
                return refusal(&format!("{old} is missing"));
            }
        }
        locked.push(format!("wamn_run.{}", rename.table));
        let checks = rename.checks.iter().collect::<Vec<_>>();
        for (index, (old_column, new_column)) in rename.columns.iter().enumerate() {
            parts.push(format!(
                "ALTER TABLE wamn_run.{} RENAME COLUMN {old_column} TO {new_column};",
                rename.table
            ));
            // A table's checks follow its columns in declaration order.
            if let Some((old_check, new_check)) = checks.get(index) {
                parts.push(format!(
                    "ALTER TABLE wamn_run.{} RENAME CONSTRAINT {old_check} TO {new_check};",
                    rename.table
                ));
            }
        }

        let table = rename.table;
        if let Some(columns) = renamed.tables.get_mut(table) {
            for (old, new) in rename.columns {
                if columns.remove(*old) {
                    columns.insert((*new).to_owned());
                }
            }
        }
        renamed.checks = rename_keys(&renamed.checks, table, rename.checks)
            .into_iter()
            .map(|((owner, name), definition)| {
                let definition = if owner == table {
                    rename_words(&definition, rename.columns)
                } else {
                    definition
                };
                ((owner, name), definition)
            })
            .collect();
        renamed.column_types = rename_keys(&renamed.column_types, table, rename.columns);
        let renamed_pairs = |set: &BTreeSet<(String, String)>| {
            set.iter()
                .map(|(owner, name)| {
                    let name = if owner == table {
                        rename
                            .columns
                            .iter()
                            .find(|(old, _)| old == name)
                            .map_or_else(|| name.clone(), |(_, new)| (*new).to_owned())
                    } else {
                        name.clone()
                    };
                    (owner.clone(), name)
                })
                .collect::<BTreeSet<_>>()
        };
        renamed.non_nullable_columns = renamed_pairs(&renamed.non_nullable_columns);
        renamed.defaulted_columns = renamed_pairs(&renamed.defaulted_columns);
        renamed.indexes = renamed
            .indexes
            .iter()
            .map(|(name, definition)| (name.clone(), rename_words(definition, rename.columns)))
            .collect();
    }

    if parts.is_empty() {
        return TypeColumnCutover::None;
    }
    let sql = format!(
        "LOCK TABLE {} IN ACCESS EXCLUSIVE MODE;\n{}\n{NOT_NULL_BLOCK}",
        locked.join(", "),
        parts.join("\n")
    );
    TypeColumnCutover::Rename {
        sql: rewrite_schema(&sql, schema),
        renamed: Box::new(renamed),
    }
}
