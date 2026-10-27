use super::*;
use wamn_schema_introspection::ir::{ColumnType, ViewColumn};

fn view(columns: &[(&str, ColumnType)]) -> View {
    View::new(
        "inventory",
        "widget_report",
        columns
            .iter()
            .map(|(name, ty)| ViewColumn::new(*name, *ty))
            .collect(),
        Vec::new(),
    )
}

#[test]
fn predecessor_view_columns_allow_additions() {
    let predecessor = view(&[("id", ColumnType::Uuid)]);
    let successor = view(&[("id", ColumnType::Uuid), ("note", ColumnType::Text)]);
    require_predecessor_views(&[predecessor], &[successor]).unwrap();
}

#[test]
fn predecessor_view_column_removal_refuses() {
    let predecessor = view(&[("id", ColumnType::Uuid), ("note", ColumnType::Text)]);
    let successor = view(&[("id", ColumnType::Uuid)]);
    let error = require_predecessor_views(&[predecessor], &[successor]).unwrap_err();
    let diagnostic = error.to_string();
    assert!(
        diagnostic.contains("inventory.widget_report"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("note"), "{diagnostic}");
    assert!(diagnostic.contains("missing column"), "{diagnostic}");
}

#[test]
fn predecessor_view_type_change_refuses() {
    let predecessor = view(&[("note", ColumnType::Text)]);
    let successor = view(&[("note", ColumnType::Uuid)]);
    let error = require_predecessor_views(&[predecessor], &[successor]).unwrap_err();
    let diagnostic = error.to_string();
    assert!(
        diagnostic.contains("inventory.widget_report"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("note"), "{diagnostic}");
    assert!(diagnostic.contains("type"), "{diagnostic}");
}

#[test]
fn predecessor_view_removal_refuses() {
    let predecessor = view(&[("id", ColumnType::Uuid)]);
    let error = require_predecessor_views(&[predecessor], &[]).unwrap_err();
    let diagnostic = error.to_string();
    assert!(
        diagnostic.contains("inventory.widget_report"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("id"), "{diagnostic}");
    assert!(diagnostic.contains("missing view"), "{diagnostic}");
    require_predecessor_views(&[view(&[])], &[]).unwrap_err();
}

#[tokio::test]
async fn qualification_refuses_a_dropped_predecessor_view_column() {
    let mut fixture = fixture("view-column", false).await;
    let owner = wamn_pg_core::quote_ident(&wamn_control_provision::package_owner_role(
        "platform_fixture",
    ));
    fixture
        .source
        .batch_execute(&format!(
            "CREATE VIEW inventory.widget_report AS SELECT id, note FROM inventory.widget; \
         ALTER VIEW inventory.widget_report OWNER TO {owner};"
        ))
        .await
        .unwrap();
    fs::write(
        &fixture.suffix,
        "DROP VIEW inventory.widget_report; \
         CREATE VIEW inventory.widget_report AS SELECT id FROM inventory.widget;",
    )
    .unwrap();
    wamn_test_infrastructure::fixture_digest::write_receipt(&fixture.root.join("candidate"));
    let original = state(&mut fixture.source).await;
    let error = qualify_upgrade(request(&fixture, "view-column.json"))
        .await
        .unwrap_err();
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("inventory.widget_report"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("note"), "{diagnostic}");
    assert!(diagnostic.contains("missing column"), "{diagnostic}");
    assert!(!fixture.root.join("view-column.json").exists());
    assert_eq!(state(&mut fixture.source).await, original);
}

#[tokio::test]
async fn qualification_accepts_a_rebuilt_view_with_an_added_column() {
    let mut fixture = fixture("view-addition", false).await;
    let owner = wamn_pg_core::quote_ident(&wamn_control_provision::package_owner_role(
        "platform_fixture",
    ));
    fixture
        .source
        .batch_execute(&format!(
            "CREATE VIEW inventory.widget_report AS SELECT id, name FROM inventory.widget_maker; \
         ALTER VIEW inventory.widget_report OWNER TO {owner};"
        ))
        .await
        .unwrap();
    fs::write(
        &fixture.suffix,
        "DROP VIEW inventory.widget_report; \
         ALTER TABLE inventory.widget_maker ADD COLUMN note text; \
         CREATE VIEW inventory.widget_report AS SELECT id, name, note FROM inventory.widget_maker;",
    )
    .unwrap();
    wamn_test_infrastructure::fixture_digest::write_receipt(&fixture.root.join("candidate"));
    let original = state(&mut fixture.source).await;
    let result = qualify_upgrade(request(&fixture, "view-addition.json"))
        .await
        .expect("qualification keeps every predecessor view column and permits the new column");
    assert!(result.result.exists());
    assert_eq!(state(&mut fixture.source).await, original);
}
