//! Author one immutable gated wiring version from its document.
//!
//! The wiring document is the artifact. Developers and generators emit it whole
//! in their repositories, `catalog.wirings.graph_json` stores it, and
//! `wiring_hash` covers it — so this verb SUBMITS that artifact rather than
//! composing one from named arguments. A typed argv surface would be a second
//! authoring grammar for a document the producers already write.
//!
//! Every refusal here belongs to a validator that already exists.
//! [`WiringDocument::parse`] owns document shape,
//! [`validate_wiring_compatibility`] owns compatibility against the gate
//! scope's admitted `catalog.component_library` facts, and the
//! `catalog.wirings` constraints own storage. The CLI adds no rule of its own,
//! so a wiring authored here and a wiring promoted into this environment are
//! admitted by exactly the same predicates.
//!
//! There is no gate-report argument (wamn-0h0g.8.5.6). The gate report keys on
//! the wiring hash, which the document itself determines, so a report id in argv
//! would be a second name for an identity the artifact already carries — and
//! `catalog.wirings` no longer has a column to put it in.
//!
//! An exact resubmission converges. The same `(wiring, version)` carrying any
//! other document, hash, or gate scope refuses rather than being replaced,
//! because the stored definition is immutable.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{
    AdmittedComponent, ComponentCatalogScope, DefinitionHash, WiringDocument,
    validate_wiring_compatibility,
};

use crate::publish_release::load_component_facts;

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

const INSERT_WIRING_SQL: &str = "\
INSERT INTO catalog.wirings (\
       tenant_id, catalog_id, wiring_id, version, gated_catalog_version, \
       graph_json, wiring_hash\
     ) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7) \
ON CONFLICT DO NOTHING";

const EXACT_WIRING_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM catalog.wirings \
     WHERE tenant_id = $1 AND catalog_id = $2 AND wiring_id = $3 AND version = $4 \
       AND gated_catalog_version = $5 AND graph_json = $6::text::jsonb \
       AND wiring_hash = $7\
    )";

/// Stable prefix every wiring-authorship refusal renders with.
pub const WIRING_AUTHORSHIP_REFUSAL: &str = "wiring-authorship-refused";

/// Stable predicate that refused one wiring authorship.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorWiringErrorKind {
    Storage,
    Document,
    Gate,
    Conflict,
}

impl AuthorWiringErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Document => "document",
            Self::Gate => "gate",
            Self::Conflict => "conflict",
        }
    }
}

/// Contextual refusal from the wiring-authorship boundary.
#[derive(Debug)]
pub struct AuthorWiringError {
    kind: AuthorWiringErrorKind,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl AuthorWiringError {
    fn new(kind: AuthorWiringErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        kind: AuthorWiringErrorKind,
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Stable refusal class for callers that must not match display text.
    pub const fn kind(&self) -> AuthorWiringErrorKind {
        self.kind
    }
}

impl std::fmt::Display for AuthorWiringError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{WIRING_AUTHORSHIP_REFUSAL} ({}): {}",
            self.kind.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for AuthorWiringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// One wiring version's identity and gate coordinates.
///
/// The wiring id and version are the document's own, never a second copy in
/// argv that could disagree with the bytes the hash covers.
#[derive(Clone, Copy, Debug)]
pub struct AuthorWiringRequest<'a> {
    pub tenant_id: &'a str,
    pub catalog_id: &'a str,
    /// Applied catalog version whose admitted component facts gate the wiring.
    pub gated_catalog_version: i32,
    pub document: &'a WiringDocument,
}

/// Arguments for the wiring-authorship verb.
#[derive(Debug, Args)]
pub struct AuthorWiringArgs {
    /// Owner URL to the project-environment database holding the catalog facts.
    #[arg(long)]
    pub database_url: String,

    /// Tenant claim carried by the authored wiring.
    #[arg(long)]
    pub tenant: String,

    /// Catalog identity the wiring is authored into.
    #[arg(long)]
    pub catalog_id: String,

    /// Applied catalog version whose component facts gate this wiring.
    #[arg(long)]
    pub gated_catalog_version: u32,

    /// The wiring document to submit; it carries its own id and version.
    #[arg(long)]
    pub wiring_document: PathBuf,
}

/// Author one gated wiring version and print its definition hash.
pub async fn run(args: AuthorWiringArgs) -> anyhow::Result<()> {
    let document = read_wiring_document(&args.wiring_document)?;
    let gated_catalog_version = i32::try_from(args.gated_catalog_version)
        .context("gated-catalog-version exceeds the PostgreSQL integer carrier")?;
    let request = AuthorWiringRequest {
        tenant_id: &args.tenant,
        catalog_id: &args.catalog_id,
        gated_catalog_version,
        document: &document,
    };

    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to the authoring project environment")?;
    let connection_task = tokio::spawn(connection);
    let authored = author_in_transaction(&mut client, &request).await;
    match authored {
        Ok(hash) => {
            drop(client);
            connection_task
                .await
                .context("join the wiring authorship connection")?
                .context("drive the wiring authorship connection")?;
            println!("{hash}");
            Ok(())
        }
        Err(error) => {
            connection_task.abort();
            Err(error)
        }
    }
}

async fn author_in_transaction(
    client: &mut Client,
    request: &AuthorWiringRequest<'_>,
) -> anyhow::Result<DefinitionHash> {
    let transaction = client
        .transaction()
        .await
        .context("begin the wiring authorship")?;
    let hash = author_wiring(&transaction, request).await?;
    transaction
        .commit()
        .await
        .context("commit the wiring authorship")?;
    Ok(hash)
}

/// Read one authored wiring document through the production document reader.
///
/// [`WiringDocument::parse`] is the only shape rule applied: its typed
/// [`wamn_catalog::CatalogIdentityError`] is carried as this refusal's source
/// so the operator sees the validator's own words.
pub fn read_wiring_document(path: &Path) -> Result<WiringDocument, AuthorWiringError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuthorWiringError::with_source(
            AuthorWiringErrorKind::Document,
            format!("read wiring document {}", path.display()),
            error,
        )
    })?;
    let value = serde_json::from_slice(&bytes).map_err(|error| {
        AuthorWiringError::with_source(
            AuthorWiringErrorKind::Document,
            format!("wiring document {} is not JSON", path.display()),
            error,
        )
    })?;
    WiringDocument::parse(&value).map_err(|error| {
        AuthorWiringError::with_source(
            AuthorWiringErrorKind::Document,
            format!("wiring document {} is not a valid wiring", path.display()),
            error,
        )
    })
}

/// Gate one document against the scope's admitted facts and derive its hash.
///
/// The hash the caller stores is produced only on the far side of the
/// production gate, so no authored row can reach `catalog.wirings` ungated.
pub fn gate_wiring_document(
    document: &WiringDocument,
    scope: &ComponentCatalogScope,
    components: &[AdmittedComponent],
) -> Result<DefinitionHash, AuthorWiringError> {
    validate_wiring_compatibility(document, scope, components).map_err(|error| {
        AuthorWiringError::with_source(
            AuthorWiringErrorKind::Gate,
            format!(
                "wiring {:?} version {} is not compatible with catalog version {} component facts",
                document.wiring_id, document.version, scope.catalog_version
            ),
            error,
        )
    })?;
    Ok(document.wiring_hash())
}

/// Gate and append one immutable wiring definition in the caller's transaction.
pub async fn author_wiring(
    transaction: &Transaction<'_>,
    request: &AuthorWiringRequest<'_>,
) -> Result<DefinitionHash, AuthorWiringError> {
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&request.tenant_id])
        .await
        .map_err(|error| storage("claim the authoring tenant", error))?;

    let catalog_version = u32::try_from(request.gated_catalog_version).map_err(|error| {
        AuthorWiringError::with_source(
            AuthorWiringErrorKind::Document,
            format!(
                "gated-catalog-version {} is outside the gate scope width",
                request.gated_catalog_version
            ),
            error,
        )
    })?;
    let scope = ComponentCatalogScope {
        tenant_id: request.tenant_id.to_string(),
        catalog_id: request.catalog_id.to_string(),
        catalog_version,
    };
    let components = load_component_facts(transaction, &scope)
        .await
        .map_err(|error| {
            AuthorWiringError::with_source(
                AuthorWiringErrorKind::Storage,
                "read the gate scope's admitted component facts",
                error,
            )
        })?;
    let wiring_hash = gate_wiring_document(request.document, &scope, &components)?;

    let version = i32::try_from(request.document.version).map_err(|error| {
        AuthorWiringError::with_source(
            AuthorWiringErrorKind::Document,
            format!(
                "wiring {:?} version {} exceeds the catalog storage width",
                request.document.wiring_id, request.document.version
            ),
            error,
        )
    })?;
    let graph_json = serde_json::to_string(request.document).expect("a wiring document serializes");
    let stored_hash = wiring_hash.as_str();
    let parameters: [&(dyn tokio_postgres::types::ToSql + Sync); 7] = [
        &request.tenant_id,
        &request.catalog_id,
        &request.document.wiring_id,
        &version,
        &request.gated_catalog_version,
        &graph_json,
        &stored_hash,
    ];
    transaction
        .execute(INSERT_WIRING_SQL, &parameters)
        .await
        .map_err(|error| storage("append the gated wiring definition", error))?;
    let exact: bool = transaction
        .query_one(EXACT_WIRING_SQL, &parameters)
        .await
        .map_err(|error| storage("verify the stored wiring definition", error))?
        .get(0);
    if !exact {
        return Err(AuthorWiringError::new(
            AuthorWiringErrorKind::Conflict,
            format!(
                "wiring {:?} version {} is already authored with other facts",
                request.document.wiring_id, request.document.version
            ),
        ));
    }
    Ok(wiring_hash)
}

fn storage(context: &'static str, error: tokio_postgres::Error) -> AuthorWiringError {
    AuthorWiringError::with_source(AuthorWiringErrorKind::Storage, context, error)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::error::Error as _;

    use clap::Parser as _;
    use wamn_catalog::{WiringNode, WiringTerminal};

    use super::*;

    const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

    /// Host command for the flattened argument surface under test.
    #[derive(Debug, clap::Parser)]
    struct AuthorProbe {
        #[command(flatten)]
        args: AuthorWiringArgs,
    }

    const COORDINATE: [&str; 8] = [
        "--database-url",
        "postgres://author.invalid/env",
        "--tenant",
        "tenant-a",
        "--catalog-id",
        "orders",
        "--gated-catalog-version",
        "3",
    ];

    fn parse(submission: &[&str]) -> Result<AuthorWiringArgs, clap::Error> {
        let mut argv = vec!["author-wiring"];
        argv.extend_from_slice(&COORDINATE);
        argv.extend_from_slice(submission);
        AuthorProbe::try_parse_from(argv).map(|probe| probe.args)
    }

    fn scope() -> ComponentCatalogScope {
        ComponentCatalogScope {
            tenant_id: "tenant-a".to_owned(),
            catalog_id: "orders".to_owned(),
            catalog_version: 3,
        }
    }

    fn document() -> WiringDocument {
        WiringDocument::new(
            "orders",
            1,
            "node",
            BTreeMap::from([(
                "node".to_owned(),
                WiringNode {
                    component: "http-request".to_owned(),
                    interface_version: "0.1".to_owned(),
                    operation: "call".to_owned(),
                    params: BTreeMap::new(),
                    terminal: Some(WiringTerminal::Respond),
                },
            )]),
            Vec::new(),
            Vec::new(),
        )
        .expect("the fixture wiring is structurally valid")
    }

    fn admitted(component: &str) -> AdmittedComponent {
        AdmittedComponent {
            scope: scope(),
            component: component.to_owned(),
            interface_version: "0.1".to_owned(),
            operation: "call".to_owned(),
            component_digest: format!("sha256:{}", "1".repeat(64)),
            imports: Vec::new(),
            imports_fingerprint: format!("sha256:{}", "6".repeat(64)),
            effects: Vec::new(),
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            parameters: Vec::new(),
        }
    }

    /// The document is the WHOLE submission, and argv adds nothing to it.
    ///
    /// The gate-report argument this used to require is gone (wamn-0h0g.8.5.6):
    /// the report keys on the wiring hash the document itself determines, so
    /// there is no report id left for a caller to supply or mis-supply.
    #[test]
    fn the_document_is_the_whole_artifact_and_argv_restates_nothing() {
        let complete =
            parse(&["--wiring-document", "wiring.json"]).expect("the submission surface parses");
        assert_eq!(complete.wiring_document, PathBuf::from("wiring.json"));

        let refusals: [Vec<&str>; 3] = [
            // There is no artifact to submit.
            vec![],
            // The retired report argument is REFUSED, not ignored: a caller
            // still passing it is asking for an identity that no longer exists,
            // and silently accepting it would suggest it still meant something.
            vec![
                "--wiring-document",
                "wiring.json",
                "--gate-report-id",
                "gate-2026-08-23",
            ],
            // The wiring id and version are the document's; argv cannot restate
            // them, so a second authoring grammar cannot start here.
            vec!["--wiring-document", "wiring.json", "--wiring-id", "orders"],
        ];
        for refused in refusals {
            assert!(parse(&refused).is_err(), "accepted {refused:?}");
        }
    }

    /// The stored row carries ONE identity for the definition.
    ///
    /// Scanned over the relation's own DECLARATION rather than the whole
    /// artifact: the section header above it still names the collapsed column to
    /// say what happened to it, and a whole-file scan would read that history as
    /// the column's return.
    #[test]
    fn the_authored_column_set_carries_no_second_identifier() {
        assert!(CATALOG_SCHEMA.contains("CREATE TRIGGER wirings_immutable"));
        assert!(CATALOG_SCHEMA.contains("CREATE TRIGGER wirings_delete_immutable"));
        let declaration = CATALOG_SCHEMA
            .split_once("CREATE TABLE catalog.wirings (")
            .expect("the catalog schema declares the wirings relation")
            .1
            .split_once("\n);")
            .expect("the wirings declaration is terminated")
            .0;
        assert!(
            declaration.contains("wiring_hash     text NOT NULL"),
            "the wirings declaration no longer carries its content hash"
        );
        assert!(
            !declaration.contains("gate_report_id"),
            "catalog.wirings regrew the collapsed second identifier"
        );
    }

    #[test]
    fn an_unreadable_document_refuses_before_any_connection() {
        let missing = read_wiring_document(Path::new("no-such-wiring-document.json"))
            .expect_err("an absent document refuses");
        assert_eq!(missing.kind(), AuthorWiringErrorKind::Document);
        assert!(
            format!("{missing}").starts_with(WIRING_AUTHORSHIP_REFUSAL),
            "refusal is unlabelled: {missing}"
        );
    }

    #[test]
    fn an_invalid_document_refuses_in_the_document_validators_own_words() {
        // Serde-valid and validator-invalid: the entry names a node the graph
        // does not declare, which only `WiringDocument::parse` refuses.
        let mut wire = serde_json::to_value(document()).expect("the fixture serializes");
        wire["entry"] = serde_json::json!("absent");
        let path =
            std::env::temp_dir().join(format!("wamn-author-wiring-{}.json", std::process::id()));
        std::fs::write(
            &path,
            serde_json::to_vec(&wire).expect("the invalid document serializes"),
        )
        .expect("write the invalid wiring document");
        let refusal =
            read_wiring_document(&path).expect_err("a graph the router cannot enter refuses");
        std::fs::remove_file(&path).expect("remove the invalid wiring document");

        assert_eq!(refusal.kind(), AuthorWiringErrorKind::Document);
        let validator = refusal
            .source()
            .expect("the document refusal is carried verbatim")
            .to_string();
        assert_eq!(
            validator,
            "wiring entry names node \"absent\", which the document does not declare"
        );
    }

    #[test]
    fn the_gate_is_the_only_producer_of_the_stored_hash() {
        let document = document();
        let hash = gate_wiring_document(&document, &scope(), &[admitted("http-request")])
            .expect("a wiring over admitted facts gates");
        assert_eq!(hash, document.wiring_hash());

        // The production gate, not a CLI rule, decides this: the node names a
        // component the gate scope has no admitted fact for.
        let refusal = gate_wiring_document(&document, &scope(), &[admitted("transform")])
            .expect_err("a wiring over absent component facts refuses");
        assert_eq!(refusal.kind(), AuthorWiringErrorKind::Gate);
        let gate = refusal
            .source()
            .expect("the gate refusal is carried verbatim")
            .to_string();
        assert!(
            gate.contains("names missing component \"http-request\""),
            "the gate's own words were not surfaced: {gate}"
        );
    }

    #[test]
    fn the_written_row_is_the_documents_own_identity() {
        for column in [
            "tenant_id",
            "catalog_id",
            "wiring_id",
            "version",
            "gated_catalog_version",
            "graph_json",
            "wiring_hash",
        ] {
            assert!(INSERT_WIRING_SQL.contains(column), "insert omits {column}");
            assert!(
                EXACT_WIRING_SQL.contains(column),
                "exactness omits {column}"
            );
        }
        // Immutable storage: a conflicting resubmission refuses, and nothing
        // this verb runs can replace a stored definition.
        assert!(INSERT_WIRING_SQL.contains("ON CONFLICT DO NOTHING"));
        assert!(!INSERT_WIRING_SQL.contains("DO UPDATE"));
        for statement in [INSERT_WIRING_SQL, EXACT_WIRING_SQL] {
            assert!(!statement.contains("UPDATE catalog.wirings"));
            assert!(!statement.contains("DELETE FROM catalog.wirings"));
        }
    }
}
