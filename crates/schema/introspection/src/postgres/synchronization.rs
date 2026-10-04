//! Exact catalog verification for caller-authorized synchronization pairs.

use std::collections::{BTreeMap, BTreeSet};

use tokio_postgres::GenericClient;

use crate::migration_policy::{StageRelation, StageSynchronization, validate_stage_trigger_body};

use super::{
    LOG_TRIGGER, PostgresIntrospectionError, PostgresIntrospectionErrorType, STAMP_TRIGGER,
    TRIGGERS_SQL, VERSION_BUMP_TRIGGER, VERSION_NOTE_TRIGGER, database_error, postgres_type,
    refusal, relation_is_excluded,
};

/// Expected artifact bytes plus caller-established package and writable-field authority.
///
/// The caller establishes package ownership and validates source-field ownership
/// before constructing this value. A shared PostgreSQL role is not package authority.
#[derive(Debug, Clone)]
pub struct StageSynchronizationExpectation {
    pub definition: StageSynchronization,
    pub writable_columns: Vec<String>,
    pub owner_role: String,
}

const RELATION: &str = "SELECT r.oid, owner.rolname::text AS owner, \
    r.relkind = 'r' AND NOT r.relispartition AND NOT EXISTS \
      (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhrelid=r.oid OR i.inhparent=r.oid) AS ordinary \
    FROM pg_catalog.pg_class r JOIN pg_catalog.pg_namespace n ON n.oid=r.relnamespace \
    JOIN pg_catalog.pg_roles owner ON owner.oid=r.relowner \
    WHERE n.nspname=$1 AND r.relname=$2";
const COLUMNS: &str = "SELECT a.attname::text, pg_catalog.format_type(a.atttypid,NULL) AS type \
    FROM pg_catalog.pg_attribute a WHERE a.attrelid=$1 AND a.attnum>0 AND NOT a.attisdropped";
const FUNCTION: &str = "SELECT p.oid, owner.rolname::text AS owner, p.prosrc, \
    p.prokind='f' AND l.lanname='plpgsql' AND p.prorettype='pg_catalog.trigger'::pg_catalog.regtype \
    AND p.pronargs=0 AND p.proallargtypes IS NULL AND p.proargmodes IS NULL \
    AND p.proargnames IS NULL AND p.pronargdefaults=0 AND p.proargdefaults IS NULL \
    AND p.provariadic=0 AND NOT p.proretset AND NOT p.prosecdef AND NOT p.proisstrict \
    AND NOT p.proleakproof AND p.provolatile='v' AND p.proparallel='u' \
    AND p.proconfig IS NULL AND p.proacl IS NULL AND p.prosupport=0 \
    AND p.procost=100 AND p.prorows=0 AND p.probin IS NULL AND p.prosqlbody IS NULL \
    AND p.protrftypes IS NULL AS exact_options \
    FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace \
    JOIN pg_catalog.pg_language l ON l.oid=p.prolang \
    JOIN pg_catalog.pg_roles owner ON owner.oid=p.proowner \
    WHERE n.nspname=$1 AND p.proname=$2";
const TRIGGER: &str = "SELECT t.tgfoid, \
    t.tgtype=23 AND t.tgenabled='O' AND NOT t.tgisinternal AND t.tgparentid=0 \
    AND t.tgnargs=0 AND pg_catalog.octet_length(t.tgargs)=0 AND t.tgattr::text='' \
    AND t.tgqual IS NULL AND t.tgconstraint=0 AND NOT t.tgdeferrable \
    AND NOT t.tginitdeferred AND t.tgoldtable IS NULL AND t.tgnewtable IS NULL AS exact_options \
    FROM pg_catalog.pg_trigger t WHERE t.tgrelid=$1 AND t.tgname=$2";

/// Verify each supplied pair against actual bodies, attributes, types, and owners.
///
/// This works inside the caller's transaction. The catalog reader additionally
/// checks that no unlisted application routine or user trigger exists.
pub async fn verify_stage_synchronizations(
    client: &impl GenericClient,
    expected: &[StageSynchronizationExpectation],
) -> Result<(), PostgresIntrospectionError> {
    let mut functions = BTreeSet::new();
    let mut triggers = BTreeSet::new();
    for expectation in expected {
        let definition = &expectation.definition;
        if !functions.insert((&definition.schema, &definition.function))
            || !triggers.insert((
                &definition.schema,
                &definition.relation,
                &definition.trigger,
            ))
        {
            return Err(error(
                definition,
                "duplicate expected synchronization identity",
            ));
        }
        if platform_trigger(&definition.trigger) {
            return Err(error(
                definition,
                "expected trigger name is reserved for the platform",
            ));
        }
        verify_pair(client, expectation).await?;
    }
    Ok(())
}

async fn verify_pair(
    client: &impl GenericClient,
    expectation: &StageSynchronizationExpectation,
) -> Result<(), PostgresIntrospectionError> {
    let definition = &expectation.definition;
    let relation = client
        .query_opt(RELATION, &[&definition.schema, &definition.relation])
        .await
        .map_err(|source| database_error("read synchronization relation", source))?
        .ok_or_else(|| error(definition, "expected synchronization relation is missing"))?;
    if !relation.get::<_, bool>("ordinary")
        || relation.get::<_, String>("owner") != expectation.owner_role
    {
        return Err(error(
            definition,
            "synchronization relation must be ordinary and have the expected owner",
        ));
    }
    let relation_oid: u32 = relation.get("oid");
    let routines = client
        .query(FUNCTION, &[&definition.schema, &definition.function])
        .await
        .map_err(|source| database_error("read synchronization function", source))?;
    let [routine] = routines.as_slice() else {
        return Err(error(
            definition,
            "expected synchronization function is missing or overloaded",
        ));
    };
    if routine.get::<_, String>("owner") != expectation.owner_role {
        return Err(error(
            definition,
            "synchronization function owner differs from the relation and expected owner",
        ));
    }
    if !routine.get::<_, bool>("exact_options") {
        return Err(error(
            definition,
            "synchronization function attributes differ from the fixed SECURITY INVOKER plpgsql trigger function declaration",
        ));
    }
    if routine.get::<_, String>("prosrc") != definition.body {
        return Err(error(
            definition,
            "synchronization function body differs from the inspected artifact",
        ));
    }
    let columns = client
        .query(COLUMNS, &[&relation_oid])
        .await
        .map_err(|source| database_error("read synchronization column types", source))?;
    let mut owned = StageRelation {
        schema: definition.schema.clone(),
        name: definition.relation.clone(),
        columns: BTreeMap::new(),
    };
    for column in columns {
        let name: String = column.get(0);
        let type_name: String = column.get(1);
        let column_type = postgres_type(&type_name).map_err(|_| {
            error(
                definition,
                format!("synchronization column {name} has unsupported type {type_name}"),
            )
        })?;
        owned.columns.insert(name, column_type);
    }
    validate_stage_trigger_body(
        "synchronization.sql",
        &definition.body,
        &owned,
        &expectation.writable_columns,
    )
    .map_err(|source| {
        error(
            definition,
            format!("actual synchronization row types refuse the artifact body: {source}"),
        )
    })?;
    let trigger = client
        .query_opt(TRIGGER, &[&relation_oid, &definition.trigger])
        .await
        .map_err(|source| database_error("read synchronization trigger", source))?
        .ok_or_else(|| error(definition, "expected synchronization trigger is missing"))?;
    if trigger.get::<_, u32>("tgfoid") != routine.get::<_, u32>("oid")
        || !trigger.get::<_, bool>("exact_options")
    {
        return Err(error(
            definition,
            "synchronization trigger differs from BEFORE INSERT OR UPDATE FOR EACH ROW, enabled, argument-free invocation of the expected function",
        ));
    }
    Ok(())
}

pub(super) fn validate_expected_scope(
    schemas: &[String],
    excluded: &[(&str, &str)],
    expected: &[StageSynchronizationExpectation],
) -> Result<(), PostgresIntrospectionError> {
    for expectation in expected {
        let definition = &expectation.definition;
        if !schemas.contains(&definition.schema)
            || relation_is_excluded(excluded, &definition.schema, &definition.relation)
        {
            return Err(error(
                definition,
                "expected synchronization is outside the included application relations",
            ));
        }
    }
    Ok(())
}

pub(super) async fn verify_catalog_synchronizations(
    client: &impl GenericClient,
    schemas: &[String],
    excluded: &[(&str, &str)],
    expected: &[StageSynchronizationExpectation],
) -> Result<(), PostgresIntrospectionError> {
    verify_stage_synchronizations(client, expected).await?;
    let routines = client.query("SELECT n.nspname::text, p.proname::text FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname=ANY($1::text[])", &[&schemas]).await
        .map_err(|source| database_error("read complete synchronization routine set", source))?;
    for routine in routines {
        let schema: String = routine.get(0);
        let name: String = routine.get(1);
        if !expected
            .iter()
            .any(|value| value.definition.schema == schema && value.definition.function == name)
        {
            return Err(refusal(
                PostgresIntrospectionErrorType::UnsupportedRoutine,
                Some(&schema),
                Some(&name),
                "routine is absent from the exact expected synchronization set",
            ));
        }
    }
    let triggers = client
        .query(TRIGGERS_SQL, &[&schemas])
        .await
        .map_err(|source| database_error("read complete synchronization trigger set", source))?;
    for trigger in triggers {
        let schema: String = trigger.get("schema_name");
        let relation: String = trigger.get("table_name");
        let name: String = trigger.get("trigger_name");
        if !platform_trigger(&name)
            && !relation_is_excluded(excluded, &schema, &relation)
            && !expected.iter().any(|value| {
                value.definition.schema == schema
                    && value.definition.relation == relation
                    && value.definition.trigger == name
            })
        {
            return Err(refusal(
                PostgresIntrospectionErrorType::UnsupportedTrigger,
                Some(&schema),
                Some(&format!("{relation}.{name}")),
                "trigger is absent from the exact expected synchronization set",
            ));
        }
    }
    Ok(())
}

fn platform_trigger(name: &str) -> bool {
    [
        STAMP_TRIGGER,
        LOG_TRIGGER,
        VERSION_NOTE_TRIGGER,
        VERSION_BUMP_TRIGGER,
    ]
    .contains(&name)
}

fn error(
    definition: &StageSynchronization,
    detail: impl Into<Box<str>>,
) -> PostgresIntrospectionError {
    refusal(
        PostgresIntrospectionErrorType::UnsupportedTrigger,
        Some(&definition.schema),
        Some(&format!(
            "{}.{} / {}()",
            definition.relation, definition.trigger, definition.function
        )),
        detail,
    )
}
