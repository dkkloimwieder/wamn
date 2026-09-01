use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnDefault, ColumnGeneration, ColumnType, Constraint, ForeignKeyAction,
    ForeignKeyColumn, IdentityMode, Index, IndexColumn, IndexDirection, IrErrorKind, Table,
    postgres_default, postgres_type,
};

fn complete_ir(reverse_collections: bool) -> CatalogIr {
    let mut purchase_order_columns = vec![
        Column::new(
            "status",
            ColumnType::Text,
            false,
            Some(ColumnDefault::TextOpen),
            None,
        ),
        Column::new(
            "row_version",
            ColumnType::Int64,
            false,
            Some(ColumnDefault::Int64One),
            None,
        ),
        Column::new("ratio", ColumnType::Float64, false, None, None),
        Column::new("payload", ColumnType::Bytes, false, None, None),
        Column::new("metadata", ColumnType::Json, true, None, None),
        Column::new(
            "id",
            ColumnType::Uuid,
            false,
            Some(ColumnDefault::GenRandomUuid),
            None,
        ),
        Column::new("enabled", ColumnType::Boolean, false, None, None),
        Column::new(
            "created_at",
            ColumnType::Timestamptz,
            false,
            Some(ColumnDefault::CurrentTimestamp),
            None,
        ),
        Column::new("count", ColumnType::Int32, false, None, None),
        Column::new(
            "amount",
            ColumnType::Numeric,
            false,
            Some(ColumnDefault::NumericZero),
            None,
        ),
        Column::new(
            "sequence_id",
            ColumnType::Int64,
            false,
            None,
            Some(ColumnGeneration::Identity {
                mode: IdentityMode::Always,
            }),
        ),
        Column::new(
            "status_key",
            ColumnType::Text,
            false,
            None,
            Some(ColumnGeneration::stored("lower(status)")),
        ),
    ];
    let mut purchase_order_constraints = vec![
        Constraint::check("purchase_order_amount_check", "(amount >= (0)::numeric)").unwrap(),
        Constraint::unique("purchase_order_status_id_key", ["status", "id"]).unwrap(),
        Constraint::primary_key("purchase_order_id_pkey", ["id"]).unwrap(),
    ];
    let mut purchase_order_indexes = vec![
        Index::new(
            "purchase_order_status_idx",
            vec![IndexColumn::new("status", IndexDirection::Asc)],
        )
        .unwrap(),
        Index::new(
            "purchase_order_created_at_idx",
            vec![IndexColumn::new("created_at", IndexDirection::Desc)],
        )
        .unwrap(),
    ];

    let mut line_columns = vec![
        Column::new(
            "id",
            ColumnType::Uuid,
            false,
            Some(ColumnDefault::GenRandomUuid),
            None,
        ),
        Column::new("purchase_order_id", ColumnType::Uuid, false, None, None),
    ];
    let mut line_constraints = vec![
        Constraint::foreign_key(
            "purchase_order_line_purchase_order_id_fkey",
            vec![ForeignKeyColumn::new("purchase_order_id", "id")],
            "receiving",
            "purchase_order",
            ForeignKeyAction::NoAction,
            ForeignKeyAction::Cascade,
        )
        .unwrap(),
        Constraint::primary_key("purchase_order_line_id_pkey", ["id"]).unwrap(),
    ];

    if reverse_collections {
        purchase_order_columns.reverse();
        purchase_order_constraints.reverse();
        purchase_order_indexes.reverse();
        line_columns.reverse();
        line_constraints.reverse();
    }

    let purchase_order = Table::new(
        "receiving",
        "purchase_order",
        purchase_order_columns,
        purchase_order_constraints,
        purchase_order_indexes,
    );
    let purchase_order_line = Table::new(
        "receiving",
        "purchase_order_line",
        line_columns,
        line_constraints,
        vec![
            Index::new(
                "purchase_order_line_purchase_order_id_idx",
                vec![IndexColumn::new("purchase_order_id", IndexDirection::Asc)],
            )
            .unwrap(),
        ],
    );

    let tables = if reverse_collections {
        vec![purchase_order_line, purchase_order]
    } else {
        vec![purchase_order, purchase_order_line]
    };
    CatalogIr::new(tables)
}

#[test]
fn canonical_bytes_freeze_the_complete_ir() {
    let ir = complete_ir(false);
    let expected = concat!(
        r#"{"tables":[{"schema":"receiving","name":"purchase_order","columns":["#,
        r#"{"name":"amount","type":"numeric","nullable":false,"default":"numeric_zero","generation":null},"#,
        r#"{"name":"count","type":"int32","nullable":false,"default":null,"generation":null},"#,
        r#"{"name":"created_at","type":"timestamptz","nullable":false,"default":"current_timestamp","generation":null},"#,
        r#"{"name":"enabled","type":"boolean","nullable":false,"default":null,"generation":null},"#,
        r#"{"name":"id","type":"uuid","nullable":false,"default":"gen_random_uuid","generation":null},"#,
        r#"{"name":"metadata","type":"json","nullable":true,"default":null,"generation":null},"#,
        r#"{"name":"payload","type":"bytes","nullable":false,"default":null,"generation":null},"#,
        r#"{"name":"ratio","type":"float64","nullable":false,"default":null,"generation":null},"#,
        r#"{"name":"row_version","type":"int64","nullable":false,"default":"int64_one","generation":null},"#,
        r#"{"name":"sequence_id","type":"int64","nullable":false,"default":null,"generation":{"kind":"identity","mode":"always"}},"#,
        r#"{"name":"status","type":"text","nullable":false,"default":"text_open","generation":null},"#,
        r#"{"name":"status_key","type":"text","nullable":false,"default":null,"generation":{"kind":"stored","expression":"lower(status)"}}],"#,
        r#""constraints":[{"name":"purchase_order_amount_check","kind":"check","expression":"(amount >= (0)::numeric)"},"#,
        r#"{"name":"purchase_order_id_pkey","kind":"primary_key","columns":["id"]},"#,
        r#"{"name":"purchase_order_status_id_key","kind":"unique","columns":["status","id"]}],"#,
        r#""indexes":[{"name":"purchase_order_created_at_idx","columns":[{"name":"created_at","direction":"desc"}]},"#,
        r#"{"name":"purchase_order_status_idx","columns":[{"name":"status","direction":"asc"}]}]},"#,
        r#"{"schema":"receiving","name":"purchase_order_line","columns":["#,
        r#"{"name":"id","type":"uuid","nullable":false,"default":"gen_random_uuid","generation":null},"#,
        r#"{"name":"purchase_order_id","type":"uuid","nullable":false,"default":null,"generation":null}],"#,
        r#""constraints":[{"name":"purchase_order_line_id_pkey","kind":"primary_key","columns":["id"]},"#,
        r#"{"name":"purchase_order_line_purchase_order_id_fkey","kind":"foreign_key","columns":[{"column":"purchase_order_id","referenced_column":"id"}],"referenced_schema":"receiving","referenced_table":"purchase_order","on_update":"no_action","on_delete":"cascade"}],"#,
        r#""indexes":[{"name":"purchase_order_line_purchase_order_id_idx","columns":[{"name":"purchase_order_id","direction":"asc"}]}]}]}"#,
    )
    .as_bytes();

    assert_eq!(ir.canonical_json_bytes(), expected);
    assert_eq!(ir.canonical_json_bytes(), ir.canonical_json_bytes());
}

#[test]
fn canonical_bytes_ignore_input_collection_order() {
    assert_eq!(
        complete_ir(false).canonical_json_bytes(),
        complete_ir(true).canonical_json_bytes()
    );
}

#[test]
fn frozen_types_and_closed_defaults_refuse_unsupported_input() {
    let constraint_name_error = Constraint::primary_key("", ["id"]).unwrap_err();
    assert_eq!(constraint_name_error.kind(), IrErrorKind::EmptyName);
    assert_eq!(constraint_name_error.input(), "constraint");
    assert_eq!(
        constraint_name_error.to_string(),
        "constraint name must not be empty"
    );
    let index_name_error = Index::new("", Vec::new()).unwrap_err();
    assert_eq!(index_name_error.kind(), IrErrorKind::EmptyName);
    assert_eq!(index_name_error.to_string(), "index name must not be empty");

    assert_eq!(postgres_type("boolean").unwrap(), ColumnType::Boolean);
    assert_eq!(postgres_type("integer").unwrap(), ColumnType::Int32);
    assert_eq!(postgres_type("bigint").unwrap(), ColumnType::Int64);
    assert_eq!(
        postgres_type("double precision").unwrap(),
        ColumnType::Float64
    );
    assert_eq!(postgres_type("text").unwrap(), ColumnType::Text);
    assert_eq!(postgres_type("bytea").unwrap(), ColumnType::Bytes);
    assert_eq!(postgres_type("numeric").unwrap(), ColumnType::Numeric);
    assert_eq!(
        postgres_type("timestamp with time zone").unwrap(),
        ColumnType::Timestamptz
    );
    assert_eq!(postgres_type("jsonb").unwrap(), ColumnType::Json);
    assert_eq!(postgres_type("uuid").unwrap(), ColumnType::Uuid);

    let type_error = postgres_type("character varying").unwrap_err();
    assert_eq!(type_error.kind(), IrErrorKind::UnsupportedType);
    assert_eq!(type_error.input(), "character varying");
    assert_eq!(
        type_error.to_string(),
        "unsupported PostgreSQL type `character varying`"
    );

    assert_eq!(
        postgres_default(ColumnType::Uuid, "gen_random_uuid()").unwrap(),
        ColumnDefault::GenRandomUuid
    );
    assert_eq!(
        postgres_default(ColumnType::Timestamptz, "CURRENT_TIMESTAMP").unwrap(),
        ColumnDefault::CurrentTimestamp
    );
    assert_eq!(
        postgres_default(ColumnType::Text, "'open'::text").unwrap(),
        ColumnDefault::TextOpen
    );
    assert_eq!(
        postgres_default(ColumnType::Text, "'not_required'::text").unwrap(),
        ColumnDefault::TextNotRequired
    );
    for spelling in ["'pending'", "'pending'::text"] {
        assert_eq!(
            postgres_default(ColumnType::Text, spelling).unwrap(),
            ColumnDefault::TextPending
        );
    }
    assert_eq!(
        postgres_default(ColumnType::Boolean, "false").unwrap(),
        ColumnDefault::BooleanFalse
    );
    assert_eq!(
        postgres_default(ColumnType::Int64, "1").unwrap(),
        ColumnDefault::Int64One
    );
    assert_eq!(
        postgres_default(ColumnType::Numeric, "0::numeric").unwrap(),
        ColumnDefault::NumericZero
    );

    let default_error = postgres_default(ColumnType::Text, "'closed'::text").unwrap_err();
    assert_eq!(default_error.kind(), IrErrorKind::UnsupportedDefault);
    assert_eq!(default_error.column_type(), Some(ColumnType::Text));
    assert_eq!(
        default_error.to_string(),
        "unsupported PostgreSQL default `'closed'::text` for wamn:postgres type `text`"
    );
}
