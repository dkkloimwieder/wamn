use wamn_schema_introspection::migration_policy::{
    MigrationPolicyErrorType, refuse_dynamic_stage_sql,
};

#[test]
fn dynamic_directives_are_named_including_routine_bodies() {
    for (sql, directive) in [
        ("EXECUTE 'SELECT 1';", "EXECUTE"),
        ("DO $$BEGIN NULL; END$$;", "DO"),
        (
            "CREATE FUNCTION inventory.f() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$BEGIN RETURN NEW; END$$;",
            "SECURITY DEFINER",
        ),
        (
            "CREATE FUNCTION inventory.f() RETURNS trigger LANGUAGE plpgsql AS $body$BEGIN EXECUTE 'DELETE FROM catalog.packages'; RETURN NEW; END$body$;",
            "EXECUTE",
        ),
        (
            "CREATE FUNCTION inventory.f() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN EXECUTE ''SELECT 1''; RETURN NEW; END';",
            "EXECUTE",
        ),
        (
            "CREATE FUNCTION inventory.f() RETURNS void LANGUAGE sql AS $outer$CREATE FUNCTION inventory.g() RETURNS void LANGUAGE plpgsql AS $inner$BEGIN EXECUTE 'SELECT 1'; END$inner$;$outer$;",
            "EXECUTE",
        ),
    ] {
        let error = refuse_dynamic_stage_sql("stage.sql", sql.as_bytes()).unwrap_err();
        assert_eq!(error.error_type(), MigrationPolicyErrorType::RuledOperation);
        assert!(error.to_string().contains(directive), "{error}");
        assert_eq!(error.statement_index(), Some(1));
    }
}

#[test]
fn inert_text_and_static_trigger_invocation_are_not_dynamic_sql() {
    for sql in [
        "SELECT 'EXECUTE', $$DO SECURITY DEFINER$$, \"execute\"; -- DO\n/* EXECUTE /* SECURITY DEFINER */ */",
        "CREATE FUNCTION inventory.f() RETURNS trigger LANGUAGE plpgsql SECURITY INVOKER AS $$BEGIN -- EXECUTE\n/* DO */ NEW.label := 'SECURITY DEFINER'; RETURN NEW; END$$;",
        "CREATE TRIGGER mirror BEFORE INSERT OR UPDATE ON inventory.item FOR EACH ROW EXECUTE FUNCTION inventory.mirror();",
        "INSERT INTO inventory.item (id) VALUES (1) ON CONFLICT DO NOTHING;",
    ] {
        refuse_dynamic_stage_sql("stage.sql", sql.as_bytes()).unwrap();
    }
}

#[test]
fn uninspectable_routine_bodies_refuse_and_preserve_outer_statement_identity() {
    let error = refuse_dynamic_stage_sql(
        "stage.sql",
        b"SELECT 1; CREATE FUNCTION inventory.f() RETURNS void LANGUAGE plpgsql AS E'BEGIN EXECUTE \\'SELECT 1\\'; END';",
    )
    .unwrap_err();
    assert_eq!(
        error.error_type(),
        MigrationPolicyErrorType::UnsupportedStatement
    );
    assert_eq!(error.statement_index(), Some(2));
    assert!(error.to_string().contains("body encoding"));

    let error = refuse_dynamic_stage_sql(
        "stage.sql",
        b"SELECT 1; CREATE FUNCTION inventory.f() RETURNS void LANGUAGE plpgsql AS $$BEGIN NULL; EXECUTE 'SELECT 1'; END$$;",
    )
    .unwrap_err();
    assert_eq!(error.statement_index(), Some(2));
    assert!(error.to_string().contains("EXECUTE"));
}

#[test]
fn concatenated_body_strings_cannot_hide_dynamic_sql() {
    let error = refuse_dynamic_stage_sql(
        "stage.sql",
        b"CREATE FUNCTION inventory.f() RETURNS void LANGUAGE plpgsql AS 'BEGIN'\n' EXECUTE ''SELECT 1''; END';",
    ).unwrap_err();
    assert_eq!(
        error.error_type(),
        MigrationPolicyErrorType::UnsupportedStatement
    );
    assert!(
        error
            .to_string()
            .contains("concatenated stage routine bodies")
    );
}

#[test]
fn typed_stage_expressions_share_the_unchanged_default_policy() {
    use wamn_schema_introspection::ir::ColumnType;
    use wamn_schema_introspection::migration_policy::validate_stage_expression;
    for (column_type, expression) in [
        (ColumnType::Uuid, "gen_random_uuid()"),
        (ColumnType::Timestamptz, "CURRENT_TIMESTAMP"),
        (ColumnType::Text, "'now() + coalesce()'::text"),
        (ColumnType::Int32, "-42::integer"),
        (ColumnType::Boolean, "true"),
    ] {
        validate_stage_expression("stage.sql", column_type, expression, &[], &[]).unwrap();
    }
    for expression in [
        "now()",
        "upper('x')",
        "pg_catalog.now()",
        "coalesce(1, 2)",
        "1 + 1",
        "'1'::bigint",
    ] {
        let error = validate_stage_expression("stage.sql", ColumnType::Int32, expression, &[], &[])
            .unwrap_err();
        assert!(error.to_string().contains(expression), "{error}");
    }
}

#[test]
fn stage_source_names_disallowed_calls_and_arithmetic_without_reading_inert_text() {
    for (sql, name) in [
        ("SELECT now() IS NOT NULL;", "now()"),
        ("SELECT coalesce($1, '{}'::jsonb);", "coalesce()"),
        ("SELECT 1 + 1 = 2;", "+"),
        (
            "CREATE FUNCTION inventory.f() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN NEW.value := NEW.value + 1; RETURN NEW; END$$;",
            "+",
        ),
    ] {
        let error = refuse_dynamic_stage_sql("stage.sql", sql.as_bytes()).unwrap_err();
        assert!(error.to_string().contains(name), "{error}");
    }
    for sql in [
        "SELECT 'upper(1) + now()', -42; -- coalesce() + 1\n",
        "CREATE FUNCTION inventory.now() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$;",
        "CREATE TRIGGER mirror BEFORE INSERT ON inventory.item FOR EACH ROW EXECUTE FUNCTION inventory.now();",
        "SELECT gen_random_uuid();",
        "CREATE TABLE inventory.item (amount numeric(10, 2), number integer DEFAULT -1, CONSTRAINT pk PRIMARY KEY (number), CONSTRAINT ck CHECK (number > 0));",
        "CREATE INDEX item_number ON inventory.item(number);",
        "ALTER TABLE inventory.item ADD CONSTRAINT fk FOREIGN KEY (number) REFERENCES inventory.other(number);",
    ] {
        refuse_dynamic_stage_sql("stage.sql", sql.as_bytes()).unwrap();
    }
}

fn owned_stage_relations() -> Vec<wamn_schema_introspection::migration_policy::StageRelation> {
    use wamn_schema_introspection::ir::ColumnType;
    use wamn_schema_introspection::migration_policy::StageRelation;
    vec![
        StageRelation {
            schema: "inventory".into(),
            name: "item".into(),
            columns: [
                ("id".into(), ColumnType::Uuid),
                ("quantity".into(), ColumnType::Int64),
                ("label".into(), ColumnType::Text),
                ("active".into(), ColumnType::Boolean),
            ]
            .into(),
        },
        StageRelation {
            schema: "inventory".into(),
            name: "other".into(),
            columns: [("id".into(), ColumnType::Uuid)].into(),
        },
    ]
}

#[test]
fn stage_conditions_admit_owned_queries_comparisons_and_typed_parameters() {
    use wamn_schema_introspection::ir::ColumnType;
    use wamn_schema_introspection::migration_policy::validate_stage_condition;
    let relations = owned_stage_relations();
    for sql in [
        "SELECT true;",
        "SELECT $1::bigint >= -1 AND $2 IS NOT NULL;",
        "SELECT NOT EXISTS (SELECT 1 FROM inventory.item AS i WHERE i.quantity < $1);",
        "SELECT EXISTS (SELECT 1 FROM inventory.item AS i WHERE i.active AND i.label = 'now(); EXECUTE + 1');",
        "SELECT EXISTS (SELECT 1 FROM inventory.item AS i WHERE EXISTS (SELECT 1 FROM inventory.other AS o WHERE o.id = i.id));",
        "SELECT EXISTS (SELECT 1 FROM inventory.item WHERE inventory.item.quantity <= 5);",
        "SELECT 1 <> 2 OR NOT (3 = 4);",
        "SELECT gen_random_uuid() != gen_random_uuid();",
        "SELECT CURRENT_TIMESTAMP = CURRENT_TIMESTAMP;",
    ] {
        validate_stage_condition(
            "condition.sql",
            sql.as_bytes(),
            &relations,
            &[ColumnType::Int64, ColumnType::Uuid],
        )
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn stage_conditions_refuse_unowned_objects_shadowed_columns_and_unapproved_expressions() {
    use wamn_schema_introspection::ir::ColumnType;
    use wamn_schema_introspection::migration_policy::validate_stage_condition;
    let relations = owned_stage_relations();
    for (sql, refused) in [
        (
            "SELECT EXISTS (SELECT 1 FROM catalog.packages);",
            "catalog.packages",
        ),
        (
            "SELECT EXISTS (SELECT 1 FROM inventory.item AS i WHERE i.foreign_column IS NULL);",
            "i.foreign_column",
        ),
        (
            "SELECT EXISTS (SELECT 1 FROM inventory.item AS i WHERE EXISTS (SELECT 1 FROM inventory.other AS i WHERE i.quantity = 1));",
            "i.quantity",
        ),
        (
            "SELECT EXISTS (SELECT 1 FROM inventory.item AS i WHERE EXISTS (SELECT 1 FROM inventory.other AS o WHERE quantity = 1));",
            "quantity",
        ),
        ("SELECT EXISTS (SELECT 1 FROM item);", "schema-qualified"),
        ("SELECT $1::\"bigint\" = 1;", "unquoted type"),
        ("SELECT now() IS NOT NULL;", "now()"),
        ("SELECT upper('x') = 'X';", "upper()"),
        (
            "SELECT pg_catalog.gen_random_uuid() IS NOT NULL;",
            "pg_catalog.gen_random_uuid()",
        ),
        ("SELECT $1::text = '1';", "cross-type"),
        ("SELECT $3 IS NULL;", "$3"),
        ("SELECT 1 + 1 = 2;", "+"),
        ("SELECT 1; DELETE FROM inventory.item;", "exactly one"),
        (
            "SELECT EXISTS (SELECT 1 FROM inventory.item AS i JOIN inventory.other AS o ON i.id = o.id);",
            "JOIN",
        ),
        ("SELECT \"current_timestamp\" IS NULL;", "current_timestamp"),
    ] {
        let error = validate_stage_condition(
            "condition.sql",
            sql.as_bytes(),
            &relations,
            &[ColumnType::Int64, ColumnType::Uuid],
        )
        .expect_err(sql);
        assert!(error.to_string().contains(refused), "{sql}: {error}");
    }
}

#[test]
fn stage_expression_types_cover_owned_columns_and_same_type_casts() {
    use wamn_schema_introspection::ir::ColumnType;
    use wamn_schema_introspection::migration_policy::validate_stage_expression;
    let relations = owned_stage_relations();
    for (expected, sql) in [
        (ColumnType::Int64, "item.quantity::bigint"),
        (ColumnType::Text, "inventory.item.label"),
        (ColumnType::Uuid, "$1::uuid"),
        (ColumnType::Boolean, "item.id = $1"),
    ] {
        validate_stage_expression(
            "expression.sql",
            expected,
            sql,
            &relations,
            &[ColumnType::Uuid],
        )
        .unwrap();
    }
    for sql in [
        "item.foreign_column",
        "catalog.packages.name",
        "id",
        "item.quantity::integer",
    ] {
        validate_stage_expression("expression.sql", ColumnType::Int64, sql, &relations, &[])
            .expect_err(sql);
    }
}

fn stage_batch_sql() -> &'static str {
    "WITH batch AS (SELECT id FROM inventory.item WHERE label IS NULL ORDER BY id LIMIT $2 FOR UPDATE), updated AS (UPDATE inventory.item SET label = 'filled' FROM batch WHERE item.id = batch.id RETURNING item.id) SELECT $1::jsonb AS next_cursor, NOT EXISTS (SELECT 1 FROM batch) AS complete"
}

fn stage_batch_keys() -> Vec<wamn_schema_introspection::migration_policy::StageUniqueKey> {
    vec![
        wamn_schema_introspection::migration_policy::StageUniqueKey {
            schema: "inventory".into(),
            relation: "item".into(),
            column: "id".into(),
        },
    ]
}

#[test]
fn stage_batch_admits_bounded_owned_updates_with_an_actual_unique_key() {
    use wamn_schema_introspection::ir::ColumnType;
    use wamn_schema_introspection::migration_policy::validate_stage_batch;
    let relations = owned_stage_relations();
    let keys = stage_batch_keys();
    let sql = stage_batch_sql();
    for sql in [
        sql.to_owned(),
        sql.replace(
            "UPDATE inventory.item SET",
            "UPDATE inventory.item AS target SET",
        )
        .replace("WHERE item.id", "WHERE target.id")
        .replace("RETURNING item.id", "RETURNING target.id"),
        sql.replace("label = 'filled'", "label = item.label"),
        sql.replace("label = 'filled'", "label = label"),
        sql.replace("label = 'filled'", "label = 'filled', active = true"),
    ] {
        validate_stage_batch(
            "batch.sql",
            sql.as_bytes(),
            &relations,
            &[ColumnType::Json, ColumnType::Int32],
            &keys,
        )
        .unwrap();
    }
}

#[test]
fn stage_batch_refuses_unbounded_unowned_or_differently_shaped_mutations() {
    use wamn_schema_introspection::ir::ColumnType;
    use wamn_schema_introspection::migration_policy::validate_stage_batch;
    let relations = owned_stage_relations();
    let keys = stage_batch_keys();
    for (from, to, named) in [
        ("inventory.item", "catalog.packages", "catalog.packages"),
        ("ORDER BY id", "ORDER BY label", "unique key id"),
        ("LIMIT $2", "LIMIT $1", "$2"),
        ("FOR UPDATE", "FOR UPDATE SKIP LOCKED", "SKIP"),
        (
            "UPDATE inventory.item",
            "UPDATE inventory.other",
            "selected owned relation",
        ),
        (
            "SET label = 'filled'",
            "SET foreign_column = 'filled'",
            "foreign_column",
        ),
        (
            "SET label = 'filled'",
            "SET id = gen_random_uuid()",
            "unique key id",
        ),
        ("label = 'filled'", "label = upper('filled')", "upper()"),
        ("label = 'filled'", "quantity = quantity + 1", "+"),
        ("batch.id RETURNING", "batch.label RETURNING", "batch.id"),
        ("NOT EXISTS (SELECT 1 FROM batch)", "true", "not"),
        ("$1::jsonb AS", "'{}'::jsonb AS", "$"),
        (
            "FROM batch) AS complete",
            "FROM catalog.packages) AS complete",
            "batch",
        ),
    ] {
        let sql = stage_batch_sql().replace(from, to);
        let error = validate_stage_batch(
            "batch.sql",
            sql.as_bytes(),
            &relations,
            &[ColumnType::Json, ColumnType::Int32],
            &keys,
        )
        .expect_err(&sql);
        assert!(error.to_string().contains(named), "{sql}: {error}");
    }
    let error = validate_stage_batch(
        "batch.sql",
        stage_batch_sql().as_bytes(),
        &relations,
        &[ColumnType::Json, ColumnType::Int32],
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("immediate NOT NULL unique key"));
    let error = validate_stage_batch(
        "batch.sql",
        stage_batch_sql().as_bytes(),
        &relations,
        &[ColumnType::Json],
        &keys,
    )
    .unwrap_err();
    assert!(error.to_string().contains("exactly [Json, Int32]"));
}

#[test]
fn batch_cte_authority_never_leaks_into_condition_validation() {
    use wamn_schema_introspection::migration_policy::validate_stage_condition;
    let error = validate_stage_condition(
        "condition.sql",
        b"SELECT EXISTS (SELECT 1 FROM batch)",
        &owned_stage_relations(),
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("schema-qualified"));
}

#[test]
fn synchronization_body_admits_only_owned_new_row_assignments() {
    use wamn_schema_introspection::migration_policy::validate_stage_trigger_body;
    let relations = owned_stage_relations();
    let writable = vec!["label".into(), "active".into(), "quantity".into()];
    for body in [
        "BEGIN NEW.label := NEW.label; RETURN NEW; END;",
        "BEGIN NEW.label := 'mirror'; NEW.active := NEW.quantity > 0; RETURN NEW; END;",
        "BEGIN NEW.quantity := NEW.quantity::bigint; RETURN NEW; END;",
        "BEGIN /* EXECUTE DO */ NEW.label := 'EXECUTE now() + 1'; RETURN NEW; END;",
    ] {
        validate_stage_trigger_body("mirror.sql", body, &relations[0], &writable).unwrap();
    }
}

#[test]
fn synchronization_body_refuses_non_row_effects_and_non_writable_targets() {
    use wamn_schema_introspection::migration_policy::validate_stage_trigger_body;
    let relations = owned_stage_relations();
    let writable = vec![
        "label".into(),
        "active".into(),
        "quantity".into(),
        "not_owned".into(),
    ];
    for (body, named) in [
        (
            "BEGIN NEW.id := gen_random_uuid(); RETURN NEW; END;",
            "NEW.id",
        ),
        (
            "BEGIN NEW.created_at := CURRENT_TIMESTAMP; RETURN NEW; END;",
            "NEW.created_at",
        ),
        (
            "BEGIN NEW.not_owned := 'x'; RETURN NEW; END;",
            "NEW.not_owned",
        ),
        (
            "BEGIN NEW.label := OLD.label; RETURN NEW; END;",
            "old.label",
        ),
        ("BEGIN NEW.label := label; RETURN NEW; END;", "read new"),
        (
            "BEGIN NEW.label := inventory.item.label; RETURN NEW; END;",
            "inventory.item.label",
        ),
        (
            "BEGIN NEW.active := EXISTS (SELECT 1 FROM inventory.item); RETURN NEW; END;",
            "EXISTS",
        ),
        (
            "BEGIN NEW.label := upper(NEW.label); RETURN NEW; END;",
            "upper()",
        ),
        (
            "BEGIN NEW.quantity := NEW.quantity + 1; RETURN NEW; END;",
            "+",
        ),
        (
            "BEGIN NEW.label := NEW.quantity::text; RETURN NEW; END;",
            "cross-type",
        ),
        ("BEGIN NEW.label := $1; RETURN NEW; END;", "$1"),
        (
            "BEGIN NEW.label := 'x'; EXECUTE 'SELECT 1'; RETURN NEW; END;",
            "EXECUTE",
        ),
        ("BEGIN NEW.label := 'x'; RETURN NULL; END;", "NULL"),
        (
            "BEGIN NEW.label := 'x'; NEW.label := 'y'; RETURN NEW; END;",
            "duplicate",
        ),
    ] {
        let error = validate_stage_trigger_body("mirror.sql", body, &relations[0], &writable)
            .expect_err(body);
        assert!(error.to_string().contains(named), "{body}: {error}");
    }
}
