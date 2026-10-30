//! Apply's direct system SQL can write only the environment row.

use std::collections::BTreeSet;

use serde_json::Value;
use syn::{parse::Parser as _, visit::Visit as _};

#[derive(Default)]
struct SqlLiterals(Vec<String>);

impl<'ast> syn::visit::Visit<'ast> for SqlLiterals {
    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        self.0.push(literal.value());
    }

    fn visit_macro(&mut self, expression: &'ast syn::Macro) {
        let arguments = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
        if let Ok(arguments) = arguments.parse2(expression.tokens.clone()) {
            let mut nested = Self::default();
            for argument in &arguments {
                nested.visit_expr(argument);
            }
            self.0.extend(nested.0);
        }
    }
}

fn system_targets(tree: &Value, targets: &mut BTreeSet<(String, String)>) {
    match tree {
        Value::Array(items) => {
            for item in items {
                system_targets(item, targets);
            }
        }
        Value::Object(fields) => {
            for (node, value) in fields {
                let relation = match node.as_str() {
                    "InsertStmt" | "UpdateStmt" | "DeleteStmt" | "MergeStmt" => {
                        value.get("relation")
                    }
                    "TruncateStmt" => value.get("relations"),
                    "CopyStmt"
                        if value["is_from"].as_bool().or(value["isFrom"].as_bool())
                            == Some(true) =>
                    {
                        value.get("relation")
                    }
                    _ => None,
                };
                if let Some(relation) = relation {
                    collect_targets(relation, targets);
                }
                system_targets(value, targets);
            }
        }
        _ => {}
    }
}

fn collect_targets(tree: &Value, targets: &mut BTreeSet<(String, String)>) {
    if let Some(schema) = tree["schemaname"].as_str()
        && matches!(schema, "registry" | "identity")
    {
        targets.insert((schema.into(), tree["relname"].as_str().unwrap().into()));
    }
    match tree {
        Value::Array(items) => {
            for item in items {
                collect_targets(item, targets);
            }
        }
        Value::Object(fields) => {
            for value in fields.values() {
                collect_targets(value, targets);
            }
        }
        _ => {}
    }
}

#[test]
fn apply_system_statements_write_only_project_envs() {
    let mut targets = BTreeSet::new();
    for source in [
        include_str!("mod.rs"),
        include_str!("lock.rs"),
        include_str!("analyse.rs"),
        include_str!("ensure.rs"),
        include_str!("bindings.rs"),
        include_str!("expand.rs"),
        include_str!("stage.rs"),
        include_str!("clients.rs"),
        include_str!("write.rs"),
        include_str!("readiness.rs"),
        include_str!("drain.rs"),
        include_str!("contract.rs"),
        include_str!("show.rs"),
    ] {
        let mut literals = SqlLiterals::default();
        literals.visit_file(&syn::parse_file(source).unwrap());
        for sql in literals.0 {
            let parsed = pg_query::parse(&sql);
            if parsed.is_err() {
                let text = sql.to_ascii_lowercase();
                let mutation = matches!(
                    text.split_whitespace().next(),
                    Some("insert" | "update" | "delete" | "merge" | "truncate" | "copy" | "with")
                );
                assert!(
                    !mutation || !(text.contains("registry") || text.contains("identity")),
                    "cannot inspect apply's system SQL: {sql}: {parsed:?}"
                );
            }
            if let Ok(parsed) = parsed {
                let tree = serde_json::to_value(parsed.protobuf).unwrap();
                system_targets(&tree, &mut targets);
                assert!(
                    targets.iter().all(|(schema, relation)| {
                        schema == "registry" && relation == "project_envs"
                    }),
                    "apply writes another system relation: {targets:?}: {sql}"
                );
            }
        }
    }
    assert_eq!(
        targets,
        BTreeSet::from([("registry".into(), "project_envs".into())])
    );
}

#[test]
fn system_write_detection_covers_batches_and_nested_statements() {
    for sql in [
        "INSERT INTO registry.connection DEFAULT VALUES",
        "UPDATE registry.integration SET status='disabled'",
        "DELETE FROM identity.credential",
        "WITH changed AS (DELETE FROM registry.connection_generation RETURNING *) SELECT * FROM changed",
        "UPDATE registry.project_envs SET status='active'; DELETE FROM registry.integration",
        "MERGE INTO registry.connection c USING registry.project_envs e ON false WHEN MATCHED THEN DELETE",
        "TRUNCATE registry.project_envs, identity.principals",
        "COPY identity.credential FROM STDIN",
    ] {
        let parsed = pg_query::parse(sql).unwrap();
        let mut targets = BTreeSet::new();
        system_targets(
            &serde_json::to_value(parsed.protobuf).unwrap(),
            &mut targets,
        );
        assert!(
            targets
                .iter()
                .any(|(schema, relation)| { schema != "registry" || relation != "project_envs" }),
            "missed a forbidden write: {sql}"
        );
    }
    for sql in [
        "SELECT * FROM registry.connection, identity.credential",
        "COPY registry.integration TO STDOUT",
    ] {
        let parsed = pg_query::parse(sql).unwrap();
        let mut targets = BTreeSet::new();
        system_targets(
            &serde_json::to_value(parsed.protobuf).unwrap(),
            &mut targets,
        );
        assert!(targets.is_empty(), "a read was treated as a write: {sql}");
    }
}
