//! Generated PostgreSQL authority for one package's declared operations.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use wamn_execution_contract::canonical_json_bytes;
use wamn_schema_introspection::ir::CatalogIr;

use crate::generate::{CLAIM_COMMAND_COLUMN, CLAIM_KEY_COLUMN};
use crate::{CrudAction, GenerateError, GenerateErrorKind, PackageManifest};

/// Package-relative canonical data-access evidence artifact.
pub const DATA_ACCESS_OVERLAY_PATH: &str = "generated/platform-policy/data-access.json";
/// Stable PostgreSQL group role inherited by prepared App generations.
pub const DATA_ACCESS_ROLE: &str = "wamn_app";

/// Strict generated evidence consumed by the post-apply reconciliation step.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DataAccessOverlay {
    package: String,
    manifest_sha256: String,
    contract: String,
    role: String,
    schemas: Vec<String>,
    relations: Vec<DataAccessRelation>,
}

/// Exact direct ACL surface for one package application relation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DataAccessRelation {
    schema: String,
    table: String,
    all_fields: Vec<String>,
    select_fields: Vec<String>,
    insert_fields: Vec<String>,
    update_fields: Vec<String>,
    lock: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_update_field: Option<String>,
}

/// Minimal live relation shape needed to re-derive generated data authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataAccessRelationInventory {
    schema: String,
    table: String,
    fields: Vec<String>,
}

/// Installed-set GuestSql authority derived from package-owned contributions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveDataAccess {
    role: String,
    schemas: Vec<String>,
    relations: Vec<EffectiveDataAccessRelation>,
}

/// One live relation in the installed-set GuestSql authority union.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveDataAccessRelation {
    schema: String,
    table: String,
    all_fields: Vec<String>,
    select_fields: Vec<String>,
    insert_fields: Vec<String>,
    update_fields: Vec<String>,
    lock_carrier_fields: Vec<String>,
}

impl DataAccessRelationInventory {
    /// Construct a normalized relation inventory from server catalog facts.
    pub fn new(
        schema: impl Into<String>,
        table: impl Into<String>,
        mut fields: Vec<String>,
    ) -> Self {
        fields.sort();
        Self {
            schema: schema.into(),
            table: table.into(),
            fields,
        }
    }
}

impl DataAccessOverlay {
    /// Parse and validate one complete generated overlay artifact.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, GenerateError> {
        let overlay: Self = serde_json::from_slice(bytes).map_err(|source| {
            GenerateError::with_source(
                GenerateErrorKind::InvalidManifest,
                "data-access overlay does not match the closed generated shape",
                source,
            )
        })?;
        overlay.validate()?;
        if overlay.canonical_bytes() != bytes {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                "data-access overlay must use canonical compact JSON",
            ));
        }
        Ok(overlay)
    }

    /// Canonical exact bytes used for generated evidence comparison.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_json_bytes(
            &serde_json::to_value(self).expect("data-access overlay always serializes"),
        )
    }

    /// Exact package coordinate that owns this evidence.
    pub fn package(&self) -> &str {
        &self.package
    }

    /// SHA-256 of the exact strict manifest bytes used by generation.
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    /// Opaque required platform-policy contract satisfied by this overlay.
    pub fn contract(&self) -> &str {
        &self.contract
    }

    /// Stable PostgreSQL group role receiving the exact privileges.
    pub fn role(&self) -> &str {
        &self.role
    }

    /// Application schemas whose direct role ACL converges to USAGE only.
    pub fn schemas(&self) -> &[String] {
        &self.schemas
    }

    /// Complete application relation inventory ordered by schema and table.
    pub fn relations(&self) -> &[DataAccessRelation] {
        &self.relations
    }

    fn validate(&self) -> Result<(), GenerateError> {
        if self.role != DATA_ACCESS_ROLE
            || self.package.is_empty()
            || !self.package.contains('@')
            || self.manifest_sha256.len() != "sha256:".len() + 64
            || self.contract.is_empty()
            || self.schemas.is_empty()
            || self.relations.is_empty()
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                "data-access overlay identity or carrier is invalid",
            ));
        }
        let mut schemas = BTreeSet::new();
        for schema in &self.schemas {
            if !valid_identifier(schema) || !schemas.insert(schema.as_str()) {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidManifest,
                    "data-access overlay schemas must be unique singular snake_case",
                ));
            }
        }
        if !self.schemas.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                "data-access overlay schemas must use canonical order",
            ));
        }
        let mut relations = BTreeSet::new();
        for relation in &self.relations {
            if !schemas.contains(relation.schema.as_str())
                || !valid_identifier(&relation.table)
                || !relations.insert((relation.schema.as_str(), relation.table.as_str()))
                || relation.all_fields.is_empty()
            {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidManifest,
                    "data-access overlay relation identity is invalid or repeated",
                ));
            }
            if !relation.all_fields.windows(2).all(|pair| pair[0] < pair[1]) {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidManifest,
                    "data-access overlay relation fields must use canonical order",
                ));
            }
            let all = relation
                .all_fields
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            if all.len() != relation.all_fields.len()
                || !relation
                    .all_fields
                    .iter()
                    .all(|field| valid_identifier(field))
            {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidManifest,
                    "data-access overlay relation fields are invalid or repeated",
                ));
            }
            for fields in [
                &relation.select_fields,
                &relation.insert_fields,
                &relation.update_fields,
            ] {
                let unique = fields.iter().map(String::as_str).collect::<BTreeSet<_>>();
                if unique.len() != fields.len()
                    || !fields.iter().all(|field| all.contains(field.as_str()))
                    || !fields.iter().map(String::as_str).eq(relation
                        .all_fields
                        .iter()
                        .map(String::as_str)
                        .filter(|field| unique.contains(field)))
                {
                    return Err(GenerateError::new(
                        GenerateErrorKind::InvalidManifest,
                        "data-access privilege fields must follow canonical relation-field order",
                    ));
                }
            }
            let expected_carrier = relation.lock.then(|| {
                relation
                    .update_fields
                    .first()
                    .or_else(|| relation.select_fields.first())
                    .cloned()
            });
            if relation.lock && relation.lock_update_field.is_none()
                || expected_carrier.flatten() != relation.lock_update_field
            {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidManifest,
                    "data-access row lock must document its deterministic UPDATE carrier",
                ));
            }
        }
        if !self
            .relations
            .windows(2)
            .all(|pair| (&pair[0].schema, &pair[0].table) < (&pair[1].schema, &pair[1].table))
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                "data-access overlay relations must use canonical order",
            ));
        }
        Ok(())
    }
}

impl DataAccessRelation {
    pub fn schema(&self) -> &str {
        &self.schema
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn all_fields(&self) -> &[String] {
        &self.all_fields
    }

    pub fn select_fields(&self) -> &[String] {
        &self.select_fields
    }

    pub fn insert_fields(&self) -> &[String] {
        &self.insert_fields
    }

    pub fn update_fields(&self) -> &[String] {
        &self.update_fields
    }

    pub const fn lock(&self) -> bool {
        self.lock
    }

    pub fn lock_update_field(&self) -> Option<&str> {
        self.lock_update_field.as_deref()
    }

    pub fn granted_update_fields(&self) -> impl Iterator<Item = &str> {
        self.update_fields
            .iter()
            .map(String::as_str)
            .chain(self.lock_update_field.iter().map(String::as_str))
            .collect::<BTreeSet<_>>()
            .into_iter()
    }
}

impl EffectiveDataAccess {
    pub fn role(&self) -> &str {
        &self.role
    }

    pub fn schemas(&self) -> &[String] {
        &self.schemas
    }

    pub fn relations(&self) -> &[EffectiveDataAccessRelation] {
        &self.relations
    }
}

impl EffectiveDataAccessRelation {
    pub fn schema(&self) -> &str {
        &self.schema
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn all_fields(&self) -> &[String] {
        &self.all_fields
    }

    pub fn select_fields(&self) -> &[String] {
        &self.select_fields
    }

    pub fn insert_fields(&self) -> &[String] {
        &self.insert_fields
    }

    pub fn update_fields(&self) -> &[String] {
        &self.update_fields
    }
}

/// Re-derive one generated contribution from its own welded relation inventory.
pub fn validate_data_access_contribution(
    overlay: &DataAccessOverlay,
    manifest_bytes: &[u8],
) -> Result<(), GenerateError> {
    let inventory = overlay
        .relations()
        .iter()
        .map(|relation| {
            DataAccessRelationInventory::new(
                relation.schema(),
                relation.table(),
                relation.all_fields().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let expected = derive_data_access_overlay_from_inventory(&inventory, manifest_bytes)?;
    if &expected == overlay {
        Ok(())
    } else {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidManifest,
            "generated data-access contribution does not match its welded package manifest",
        ))
    }
}

/// Derive the exact GuestSql authority union for one installed package set.
pub fn derive_effective_data_access(
    inventory: &[DataAccessRelationInventory],
    overlays: &[DataAccessOverlay],
) -> Result<EffectiveDataAccess, GenerateError> {
    if overlays.is_empty() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidManifest,
            "installed data-access set must contain at least one package contribution",
        ));
    }

    let mut packages = BTreeSet::new();
    let mut schemas = BTreeSet::new();
    for overlay in overlays {
        overlay.validate()?;
        if !packages.insert(overlay.package()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                "installed data-access set repeats a package coordinate",
            ));
        }
        schemas.extend(overlay.schemas().iter().cloned());
    }

    let mut desired = BTreeMap::new();
    for relation in inventory {
        relation.validate()?;
        if schemas.contains(&relation.schema)
            && desired
                .insert(
                    (relation.schema.clone(), relation.table.clone()),
                    EffectiveDesiredRelation::new(relation),
                )
                .is_some()
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                "live installed-set relation inventory repeats a relation",
            ));
        }
    }
    for schema in &schemas {
        if !desired.keys().any(|(candidate, _)| candidate == schema) {
            return Err(GenerateError::for_object(
                GenerateErrorKind::UnknownRelation,
                "installed data-access schema has no ordinary relation inventory",
                schema.as_str(),
            ));
        }
    }

    for overlay in overlays {
        for relation in overlay.relations() {
            let key = (relation.schema().to_owned(), relation.table().to_owned());
            let target = desired.get_mut(&key).ok_or_else(|| {
                GenerateError::for_object(
                    GenerateErrorKind::UnknownRelation,
                    "generated data-access contribution references a relation absent from the installed set",
                    format!("{}.{}", relation.schema(), relation.table()),
                )
            })?;
            target.add(relation)?;
        }
    }

    let relations = desired
        .into_iter()
        .map(|((schema, table), relation)| relation.finish(schema, table))
        .collect();
    Ok(EffectiveDataAccess {
        role: DATA_ACCESS_ROLE.to_owned(),
        schemas: schemas.into_iter().collect(),
        relations,
    })
}

/// Render one exact reconciliation for the complete installed package set.
pub fn render_effective_data_access_sql(
    effective: &EffectiveDataAccess,
) -> Result<String, GenerateError> {
    if effective.role != DATA_ACCESS_ROLE
        || effective.schemas.is_empty()
        || effective.relations.is_empty()
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidManifest,
            "effective data-access authority is empty or uses the wrong carrier",
        ));
    }
    let role = quote_identifier(&effective.role);
    let mut sql = String::new();
    for schema in &effective.schemas {
        let schema = quote_identifier(schema);
        writeln!(
            sql,
            "REVOKE ALL PRIVILEGES ON SCHEMA {schema} FROM PUBLIC, {role};"
        )
        .expect("writing SQL to a String cannot fail");
        writeln!(sql, "GRANT USAGE ON SCHEMA {schema} TO {role};")
            .expect("writing SQL to a String cannot fail");
    }
    for relation in &effective.relations {
        let target = format!(
            "{}.{}",
            quote_identifier(&relation.schema),
            quote_identifier(&relation.table)
        );
        writeln!(
            sql,
            "REVOKE ALL PRIVILEGES ON TABLE {target} FROM PUBLIC, {role};"
        )
        .expect("writing SQL to a String cannot fail");
        let all_fields = field_list(&relation.all_fields);
        for privilege in ["SELECT", "INSERT", "UPDATE", "REFERENCES"] {
            writeln!(
                sql,
                "REVOKE {privilege} ({all_fields}) ON TABLE {target} FROM PUBLIC, {role};"
            )
            .expect("writing SQL to a String cannot fail");
        }
        grant_columns(&mut sql, "SELECT", &relation.select_fields, &target, &role);
        grant_columns(&mut sql, "INSERT", &relation.insert_fields, &target, &role);
        for carrier in &relation.lock_carrier_fields {
            writeln!(
                sql,
                "-- FOR KEY SHARE carrier on {target}: UPDATE ({}) is PostgreSQL lock mechanics, not declared DML update authority.",
                quote_identifier(carrier)
            )
            .expect("writing SQL to a String cannot fail");
        }
        grant_columns(&mut sql, "UPDATE", &relation.update_fields, &target, &role);
    }
    Ok(sql)
}

struct EffectiveDesiredRelation {
    all_fields: Vec<String>,
    select: BTreeSet<String>,
    insert: BTreeSet<String>,
    update: BTreeSet<String>,
    lock_carriers: BTreeSet<String>,
}

impl EffectiveDesiredRelation {
    fn new(relation: &DataAccessRelationInventory) -> Self {
        Self {
            all_fields: relation.fields.clone(),
            select: BTreeSet::new(),
            insert: BTreeSet::new(),
            update: BTreeSet::new(),
            lock_carriers: BTreeSet::new(),
        }
    }

    fn add(&mut self, relation: &DataAccessRelation) -> Result<(), GenerateError> {
        if let Some(field) = relation
            .all_fields()
            .iter()
            .find(|field| !self.all_fields.contains(*field))
        {
            return Err(GenerateError::for_object(
                GenerateErrorKind::UnknownColumn,
                "generated data-access contribution references a field absent from the installed set",
                format!("{}.{}.{}", relation.schema(), relation.table(), field),
            ));
        }
        self.select.extend(relation.select_fields().iter().cloned());
        self.insert.extend(relation.insert_fields().iter().cloned());
        self.update.extend(relation.update_fields().iter().cloned());
        if let Some(carrier) = relation.lock_update_field() {
            self.update.insert(carrier.to_owned());
            self.lock_carriers.insert(carrier.to_owned());
        }
        Ok(())
    }

    fn finish(self, schema: String, table: String) -> EffectiveDataAccessRelation {
        let ordered = |fields: &BTreeSet<String>| {
            self.all_fields
                .iter()
                .filter(|field| fields.contains(field.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        };
        EffectiveDataAccessRelation {
            schema,
            table,
            select_fields: ordered(&self.select),
            insert_fields: ordered(&self.insert),
            update_fields: ordered(&self.update),
            lock_carrier_fields: ordered(&self.lock_carriers),
            all_fields: self.all_fields,
        }
    }
}

pub(crate) fn derive_data_access_overlay(
    catalog: &CatalogIr,
    manifest_bytes: &[u8],
    manifest: &PackageManifest,
) -> Result<DataAccessOverlay, GenerateError> {
    let inventory = catalog
        .tables()
        .iter()
        .map(|table| {
            DataAccessRelationInventory::new(
                table.schema(),
                table.name(),
                table
                    .columns()
                    .iter()
                    .map(|column| column.name().to_owned())
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    derive_data_access_overlay_for_manifest(&inventory, manifest_bytes, manifest)
}

/// Re-derive the exact overlay from a verified manifest and live relation inventory.
pub fn derive_data_access_overlay_from_inventory(
    inventory: &[DataAccessRelationInventory],
    manifest_bytes: &[u8],
) -> Result<DataAccessOverlay, GenerateError> {
    let manifest = PackageManifest::from_slice(manifest_bytes)?;
    derive_data_access_overlay_for_manifest(inventory, manifest_bytes, &manifest)
}

/// Application schemas whose complete relation inventory owns ACL convergence.
pub fn data_access_schemas(manifest_bytes: &[u8]) -> Result<Vec<String>, GenerateError> {
    let manifest = PackageManifest::from_slice(manifest_bytes)?;
    application_schemas(&manifest)
}

fn derive_data_access_overlay_for_manifest(
    inventory: &[DataAccessRelationInventory],
    manifest_bytes: &[u8],
    manifest: &PackageManifest,
) -> Result<DataAccessOverlay, GenerateError> {
    let schemas = application_schemas(manifest)?;
    let schema_set = schemas.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let mut desired = BTreeMap::<(String, String), DesiredRelation>::new();
    for relation in inventory {
        relation.validate()?;
        if schema_set.contains(relation.schema.as_str())
            && desired
                .insert(
                    (relation.schema.clone(), relation.table.clone()),
                    DesiredRelation::new(relation),
                )
                .is_some()
        {
            return Err(GenerateError::for_object(
                GenerateErrorKind::InvalidManifest,
                "live data-access relation inventory repeats a relation",
                format!("{}.{}", relation.schema, relation.table),
            ));
        }
    }
    for schema in &schemas {
        if !desired.keys().any(|(candidate, _)| candidate == schema) {
            return Err(GenerateError::for_object(
                GenerateErrorKind::UnknownRelation,
                "application schema has no ordinary relation inventory",
                schema.as_str(),
            ));
        }
    }
    for model in manifest.models.values() {
        let relation = desired_relation(&mut desired, &model.schema, &model.table)?;
        for (action, operation) in &model.operations {
            match action {
                CrudAction::Get | CrudAction::Query => {
                    relation.select_all();
                }
                CrudAction::Create => {
                    relation.select_all();
                    relation
                        .insert
                        .extend(operation.writable_fields.iter().cloned());
                    // The create binds every identity from the claim instead
                    // of letting PostgreSQL default it, so it must be able to
                    // write those columns.
                    relation.insert.extend(
                        operation
                            .claim
                            .iter()
                            .flat_map(|claim| claim.identities.keys())
                            .cloned(),
                    );
                }
                CrudAction::Update => {
                    relation.select_all();
                    relation
                        .update
                        .extend(operation.writable_fields.iter().cloned());
                    relation
                        .update
                        .extend(operation.revision_field.iter().cloned());
                    relation.lock = true;
                }
                CrudAction::Delete => {
                    return Err(GenerateError::new(
                        GenerateErrorKind::InvalidOperation,
                        "generated data-access overlay has no demanded DELETE authority shape",
                    ));
                }
            }
        }
    }
    // The claim relation is the create's own mechanism state: it reads the key,
    // the canonical command and the identities it minted, and writes only the
    // first two. Nothing grants it UPDATE, because a claim is written once.
    for model in manifest.models.values() {
        let Some(claim) = model
            .operations
            .get(&CrudAction::Create)
            .and_then(|operation| operation.claim.as_ref())
        else {
            continue;
        };
        let relation = desired_relation(&mut desired, &model.schema, &claim.table)?;
        relation.select.insert(CLAIM_KEY_COLUMN.to_owned());
        relation.select.insert(CLAIM_COMMAND_COLUMN.to_owned());
        relation.select.extend(claim.identities.values().cloned());
        relation.insert.insert(CLAIM_KEY_COLUMN.to_owned());
        relation.insert.insert(CLAIM_COMMAND_COLUMN.to_owned());
    }
    for operation in manifest.custom_operations.values() {
        for declared in &operation.relations {
            let relation = desired_relation(&mut desired, &declared.schema, &declared.table)?;
            relation
                .select
                .extend(declared.select_fields.iter().cloned());
            relation
                .insert
                .extend(declared.insert_fields.iter().cloned());
            relation
                .update
                .extend(declared.update_fields.iter().cloned());
            relation.lock |= declared.lock;
        }
    }
    let relations = desired
        .into_iter()
        .map(|((schema, table), desired)| desired.finish(schema, table))
        .collect::<Result<Vec<_>, _>>()?;
    let coordinate = format!("{}@{}", manifest.package.id, manifest.package.version);
    let overlay = DataAccessOverlay {
        package: coordinate,
        manifest_sha256: sha256(manifest_bytes),
        contract: manifest.required_platform_policy_contract.id.clone(),
        role: DATA_ACCESS_ROLE.to_owned(),
        schemas,
        relations,
    };
    overlay.validate()?;
    Ok(overlay)
}

pub(crate) fn application_schemas(
    manifest: &PackageManifest,
) -> Result<Vec<String>, GenerateError> {
    let schemas = manifest
        .models
        .values()
        .map(|model| model.schema.clone())
        .chain(manifest.custom_operations.values().flat_map(|operation| {
            operation
                .relations
                .iter()
                .map(|relation| relation.schema.clone())
        }))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if schemas.is_empty() || !schemas.iter().all(|schema| valid_identifier(schema)) {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidManifest,
            "data-access manifest must name at least one valid application schema",
        ));
    }
    Ok(schemas)
}

fn desired_relation<'a>(
    desired: &'a mut BTreeMap<(String, String), DesiredRelation>,
    schema: &str,
    table: &str,
) -> Result<&'a mut DesiredRelation, GenerateError> {
    desired
        .get_mut(&(schema.to_owned(), table.to_owned()))
        .ok_or_else(|| {
            GenerateError::for_object(
                GenerateErrorKind::UnknownRelation,
                "data-access declaration references an unknown relation",
                format!("{schema}.{table}"),
            )
        })
}

impl DataAccessRelationInventory {
    fn validate(&self) -> Result<(), GenerateError> {
        if !valid_identifier(&self.schema)
            || !valid_identifier(&self.table)
            || self.fields.is_empty()
            || !self.fields.iter().all(|field| valid_identifier(field))
            || !self.fields.windows(2).all(|pair| pair[0] < pair[1])
        {
            return Err(GenerateError::for_object(
                GenerateErrorKind::InvalidManifest,
                "live data-access relation inventory is invalid",
                format!("{}.{}", self.schema, self.table),
            ));
        }
        Ok(())
    }
}

struct DesiredRelation {
    all_fields: Vec<String>,
    select: BTreeSet<String>,
    insert: BTreeSet<String>,
    update: BTreeSet<String>,
    lock: bool,
}

impl DesiredRelation {
    fn new(relation: &DataAccessRelationInventory) -> Self {
        Self {
            all_fields: relation.fields.clone(),
            select: BTreeSet::new(),
            insert: BTreeSet::new(),
            update: BTreeSet::new(),
            lock: false,
        }
    }

    fn select_all(&mut self) {
        self.select.extend(self.all_fields.iter().cloned());
    }

    fn finish(self, schema: String, table: String) -> Result<DataAccessRelation, GenerateError> {
        let declared = self
            .select
            .iter()
            .chain(&self.insert)
            .chain(&self.update)
            .collect::<BTreeSet<_>>();
        if let Some(field) = declared
            .iter()
            .find(|field| !self.all_fields.contains(*field))
        {
            return Err(GenerateError::for_object(
                GenerateErrorKind::UnknownColumn,
                "data-access declaration references an unknown field",
                format!("{schema}.{table}.{field}"),
            ));
        }
        let ordered = |fields: &BTreeSet<String>| {
            self.all_fields
                .iter()
                .filter(|field| fields.contains(field.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        };
        let select_fields = ordered(&self.select);
        let insert_fields = ordered(&self.insert);
        let update_fields = ordered(&self.update);
        let all_fields = self.all_fields;
        let lock_update_field = if self.lock {
            Some(
                update_fields
                    .first()
                    .or_else(|| select_fields.first())
                    .cloned()
                    .ok_or_else(|| {
                        GenerateError::for_object(
                            GenerateErrorKind::InvalidOperation,
                            "row-locked relation has no deterministic UPDATE carrier column",
                            format!("{schema}.{table}"),
                        )
                    })?,
            )
        } else {
            None
        };
        Ok(DataAccessRelation {
            schema,
            table,
            all_fields,
            select_fields,
            insert_fields,
            update_fields,
            lock: self.lock,
            lock_update_field,
        })
    }
}

fn grant_columns(sql: &mut String, privilege: &str, fields: &[String], target: &str, role: &str) {
    if fields.is_empty() {
        return;
    }
    writeln!(
        sql,
        "GRANT {privilege} ({}) ON TABLE {target} TO {role};",
        field_list(fields)
    )
    .expect("writing SQL to a String cannot fail");
}

fn field_list(fields: &[String]) -> String {
    fields
        .iter()
        .map(|field| quote_identifier(field))
        .collect::<Vec<_>>()
        .join(", ")
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() < 64
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || byte == b'_' || (index > 0 && byte.is_ascii_digit())
        })
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}
