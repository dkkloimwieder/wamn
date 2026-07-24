use wamn_entity_access::{
    CompareOp, EntityAccessError, EntityOperation, EntityRequest, Filter, ListOptions, Planner,
};
use wamn_pg_core::SqlValue;
use wamn_schema_model::Catalog;

const CATALOG: &str =
    include_str!("../../../schema/model/tests/fixtures/poc-receiving.catalog.json");

fn catalog() -> Catalog {
    Catalog::from_json(CATALOG).expect("catalog fixture parses")
}

#[test]
fn typed_list_preserves_the_existing_sql_and_parameter_order() {
    let request = EntityRequest {
        entity: "suppliers".to_string(),
        operation: EntityOperation::List(ListOptions {
            filters: vec![Filter::Compare {
                field: "name".to_string(),
                op: CompareOp::Eq,
                value: "O'Reilly".to_string(),
            }],
            limit: Some(20),
            ..ListOptions::default()
        }),
    };
    let catalog = catalog();
    let plan = Planner::new(&catalog).plan(&request).unwrap();
    assert_eq!(
        plan.statement().sql(),
        "SELECT \"id\", \"name\", \"contact_email\", \"standard_cost\" FROM \"suppliers\" \
         WHERE \"name\" = $1 ORDER BY \"id\" ASC LIMIT $2 OFFSET $3"
    );
    assert_eq!(
        plan.statement().params(),
        [
            SqlValue::Text("O'Reilly".to_string()),
            SqlValue::Int64(20),
            SqlValue::Int64(0),
        ]
    );
}

#[test]
fn hostile_identifier_text_never_reaches_sql() {
    let request = EntityRequest {
        entity: "suppliers\"; DROP TABLE suppliers; --".to_string(),
        operation: EntityOperation::List(ListOptions::default()),
    };
    let catalog = catalog();
    assert!(matches!(
        Planner::new(&catalog).plan(&request),
        Err(EntityAccessError::UnknownEntity(_))
    ));
}
