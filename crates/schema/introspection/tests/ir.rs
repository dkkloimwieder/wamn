use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnDefault, ColumnGeneration, ColumnType, Constraint, Exclusion,
    ExclusionAccessMethod, ExclusionElement, ExclusionKey, ForeignKeyAction, ForeignKeyColumn,
    IdentityMode, Index, IndexColumn, IndexDirection, IrErrorKind, Table, postgres_default,
    postgres_type,
};

fn complete_ir(reverse_collections: bool) -> CatalogIr {
    let mut purchase_order_columns = vec![
        Column::new(
            "status",
            ColumnType::Text,
            false,
            Some(ColumnDefault::text("open")),
            None,
        ),
        Column::new(
            "row_version",
            ColumnType::Int64,
            false,
            Some(ColumnDefault::int64(1)),
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
            Some(ColumnDefault::numeric("0")),
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
        r#"{"name":"amount","type":"numeric","nullable":false,"default":{"kind":"numeric","value":"0"},"generation":null},"#,
        r#"{"name":"count","type":"int32","nullable":false,"default":null,"generation":null},"#,
        r#"{"name":"created_at","type":"timestamptz","nullable":false,"default":{"kind":"current_timestamp"},"generation":null},"#,
        r#"{"name":"enabled","type":"boolean","nullable":false,"default":null,"generation":null},"#,
        r#"{"name":"id","type":"uuid","nullable":false,"default":{"kind":"gen_random_uuid"},"generation":null},"#,
        r#"{"name":"metadata","type":"json","nullable":true,"default":null,"generation":null},"#,
        r#"{"name":"payload","type":"bytes","nullable":false,"default":null,"generation":null},"#,
        r#"{"name":"ratio","type":"float64","nullable":false,"default":null,"generation":null},"#,
        r#"{"name":"row_version","type":"int64","nullable":false,"default":{"kind":"int64","value":1},"generation":null},"#,
        r#"{"name":"sequence_id","type":"int64","nullable":false,"default":null,"generation":{"kind":"identity","mode":"always"}},"#,
        r#"{"name":"status","type":"text","nullable":false,"default":{"kind":"text","value":"open"},"generation":null},"#,
        r#"{"name":"status_key","type":"text","nullable":false,"default":null,"generation":{"kind":"stored","expression":"lower(status)"}}],"#,
        r#""constraints":[{"name":"purchase_order_amount_check","kind":"check","expression":"(amount >= (0)::numeric)"},"#,
        r#"{"name":"purchase_order_id_pkey","kind":"primary_key","columns":["id"]},"#,
        r#"{"name":"purchase_order_status_id_key","kind":"unique","columns":["status","id"]}],"#,
        r#""indexes":[{"name":"purchase_order_created_at_idx","columns":[{"name":"created_at","direction":"desc"}]},"#,
        r#"{"name":"purchase_order_status_idx","columns":[{"name":"status","direction":"asc"}]}]},"#,
        r#"{"schema":"receiving","name":"purchase_order_line","columns":["#,
        r#"{"name":"id","type":"uuid","nullable":false,"default":{"kind":"gen_random_uuid"},"generation":null},"#,
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
        ColumnDefault::text("open")
    );
    assert_eq!(
        postgres_default(ColumnType::Text, "'not_required'::text").unwrap(),
        ColumnDefault::text("not_required")
    );
    for spelling in ["'pending'", "'pending'::text"] {
        assert_eq!(
            postgres_default(ColumnType::Text, spelling).unwrap(),
            ColumnDefault::text("pending")
        );
    }
    assert_eq!(
        postgres_default(ColumnType::Boolean, "false").unwrap(),
        ColumnDefault::boolean(false)
    );
    assert_eq!(
        postgres_default(ColumnType::Int64, "1").unwrap(),
        ColumnDefault::int64(1)
    );
    assert_eq!(
        postgres_default(ColumnType::Numeric, "0::numeric").unwrap(),
        ColumnDefault::numeric("0")
    );

    // A word nobody enumerated. Three agents wrote exactly this shape and all
    // three were refused before wamn-frru; the allowlist closes over FORMS now.
    assert_eq!(
        postgres_default(ColumnType::Text, "'scheduled'::text").unwrap(),
        ColumnDefault::text("scheduled")
    );
    assert_eq!(
        postgres_default(ColumnType::Boolean, "true").unwrap(),
        ColumnDefault::boolean(true)
    );
    assert_eq!(
        postgres_default(ColumnType::Int64, "'-12'::bigint").unwrap(),
        ColumnDefault::int64(-12)
    );
    assert_eq!(
        postgres_default(ColumnType::Numeric, "'0.00'::numeric").unwrap(),
        ColumnDefault::numeric("0.00")
    );
    // A doubled quote is one quote, and it does not end the literal.
    assert_eq!(
        postgres_default(ColumnType::Text, "'it''s'::text").unwrap(),
        ColumnDefault::text("it's")
    );

    // THE WALL THAT REMAINS. A form that is not a literal of the column's own
    // type still refuses, and each of these is a different way to miss it.
    for (column_type, expression) in [
        // an expression, not a literal
        (ColumnType::Int64, "1 + 1"),
        // a function call that is not one of the two admitted by name
        (ColumnType::Text, "upper('open')"),
        (ColumnType::Timestamptz, "now()"),
        // the right shape cast to the WRONG type
        (ColumnType::Text, "'open'::name"),
        (ColumnType::Int64, "'1'::integer"),
        // a literal of another type entirely
        (ColumnType::Boolean, "'open'"),
        (ColumnType::Int64, "'not a number'"),
        (ColumnType::Numeric, "'1.2.3'"),
        // a bare identifier
        (ColumnType::Text, "open"),
        // the two named functions on a type they do not serve
        (ColumnType::Text, "gen_random_uuid()"),
        // a type with no literal default form yet
        (ColumnType::Json, "'{}'::json"),
    ] {
        let error = postgres_default(column_type, expression).expect_err(&format!(
            "{expression} is not a default for {column_type:?}"
        ));
        assert_eq!(
            error.kind(),
            IrErrorKind::UnsupportedDefault,
            "{expression}"
        );
        assert_eq!(error.column_type(), Some(column_type), "{expression}");
    }

    let default_error = postgres_default(ColumnType::Text, "upper('open')").unwrap_err();
    assert_eq!(
        default_error.to_string(),
        "unsupported PostgreSQL default `upper('open')` for wamn:postgres type `text`"
    );
}

fn exclusion_ir(reverse_collections: bool) -> CatalogIr {
    let mut exclusions = vec![
        Exclusion::new(
            "dock_appointment_no_overlap",
            ExclusionAccessMethod::Gist,
            vec![
                ExclusionKey::new(ExclusionElement::column("dock_id"), "="),
                ExclusionKey::new(
                    ExclusionElement::expression("tstzrange(starts_at, ends_at)"),
                    "&&",
                ),
            ],
        )
        .unwrap(),
        Exclusion::new(
            "dock_appointment_one_carrier",
            ExclusionAccessMethod::Gist,
            vec![ExclusionKey::new(
                ExclusionElement::column("carrier_id"),
                "=",
            )],
        )
        .unwrap(),
    ];
    if reverse_collections {
        exclusions.reverse();
    }
    CatalogIr::new(vec![
        Table::new(
            "receiving",
            "dock_appointment",
            vec![Column::new("dock_id", ColumnType::Uuid, false, None, None)],
            vec![Constraint::primary_key("dock_appointment_dock_id_pkey", ["dock_id"]).unwrap()],
            Vec::new(),
        )
        .with_exclusions(exclusions),
    ])
}

#[test]
fn canonical_bytes_freeze_an_exclusion_constraint() {
    let expected = concat!(
        r#"{"tables":[{"schema":"receiving","name":"dock_appointment","columns":["#,
        r#"{"name":"dock_id","type":"uuid","nullable":false,"default":null,"generation":null}],"#,
        r#""constraints":[{"name":"dock_appointment_dock_id_pkey","kind":"primary_key","columns":["dock_id"]}],"#,
        r#""indexes":[],"#,
        r#""exclusions":[{"name":"dock_appointment_no_overlap","access_method":"gist","keys":["#,
        r#"{"element":"column","name":"dock_id","operator":"="},"#,
        r#"{"element":"expression","expression":"tstzrange(starts_at, ends_at)","operator":"&&"}]},"#,
        r#"{"name":"dock_appointment_one_carrier","access_method":"gist","keys":["#,
        r#"{"element":"column","name":"carrier_id","operator":"="}]}]}]}"#,
    )
    .as_bytes();

    assert_eq!(exclusion_ir(false).canonical_json_bytes(), expected);
    // Exclusion constraints normalize like every other unordered collection,
    // while the keys inside one keep the order the constraint declares.
    assert_eq!(
        exclusion_ir(true).canonical_json_bytes(),
        exclusion_ir(false).canonical_json_bytes()
    );

    let name_error = Exclusion::new("", ExclusionAccessMethod::Gist, Vec::new()).unwrap_err();
    assert_eq!(name_error.kind(), IrErrorKind::EmptyName);
    assert_eq!(
        name_error.to_string(),
        "exclusion constraint name must not be empty"
    );
}

#[test]
fn a_table_without_an_exclusion_constraint_keeps_its_canonical_bytes() {
    let json = String::from_utf8(complete_ir(false).canonical_json_bytes()).unwrap();
    assert!(!json.contains("exclusions"), "{json}");
}
