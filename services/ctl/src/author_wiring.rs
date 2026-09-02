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
//! There is no gate-report argument (wamn-0h0g.8.5.6), but there IS a gate
//! report REQUIREMENT. The report keys on the wiring hash, which the document
//! itself determines, so a report id in argv would be a second name for an
//! identity the artifact already carries — and `catalog.wirings` no longer has a
//! column to put it in. What that collapsed column can no longer say, the verb
//! says instead: [`author_wiring`] reads `wamn_run.gate_reports` under the hash
//! its own gate just produced and refuses unless a row exists there and passed.
//! The report lives in the CONTROL database and `catalog.wirings` in the PROJECT
//! database, so this is a two-connection verb; no single statement, and no
//! project-plane fact, could carry the check. Because the VERB performs the
//! read, the requirement is not caller discipline: there is no argument to omit,
//! no proof value to forge or replay, and no path through this module that
//! reaches the INSERT with the read skipped.
//!
//! An exact resubmission converges. The same `(wiring, version)` carrying any
//! other document, hash, or gate scope refuses rather than being replaced,
//! because the stored definition is immutable.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{
    AdmittedComponent, ComponentPackageScope, DefinitionHash, WiringDocument,
    validate_wiring_compatibility,
};

use crate::publish_release::load_component_facts;

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

/// Claim the tenant for the WHOLE control connection.
///
/// `wamn_run.gate_reports` is FORCE-RLS, so an unclaimed session reads zero rows
/// and would refuse every authorship as ungated. The control read runs outside
/// any explicit transaction, where the local form would expire with the
/// statement that set it, so the claim is session-scoped here — the same shape
/// [`crate::promote`] uses for a connection it opened for one tenant's work.
const CLAIM_CONTROL_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, false)";

/// Read the gate verdict recorded for exactly one document's hash.
///
/// `report_id` IS the wiring hash (wamn-0h0g.8.5.6), so this key is the whole
/// hash binding: a green report over other bytes lives under another key and is
/// simply not found here.
///
/// Params: `$1` tenant, `$2` wiring hash.
const SELECT_GATE_REPORT_SQL: &str = "\
SELECT passed FROM wamn_run.gate_reports \
 WHERE tenant_id = $1 AND wiring_hash = $2";

const INSERT_WIRING_SQL: &str = "\
INSERT INTO catalog.wirings (\
       tenant_id, package_id, package_version, wiring_id, version, graph_json, wiring_hash\
     ) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7) \
ON CONFLICT DO NOTHING";

const EXACT_WIRING_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM catalog.wirings \
     WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
       AND wiring_id = $4 AND version = $5 AND graph_json = $6::text::jsonb \
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
    /// No GREEN gate report covers this document's own hash.
    Report,
    Conflict,
}

impl AuthorWiringErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Document => "document",
            Self::Gate => "gate",
            Self::Report => "report",
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
    pub package_id: &'a str,
    pub package_version: &'a str,
    pub document: &'a WiringDocument,
}

/// Arguments for the wiring-authorship verb.
#[derive(Debug, Args)]
pub struct AuthorWiringArgs {
    /// Owner URL to the project-environment database holding the catalog facts.
    #[arg(long)]
    pub database_url: String,

    /// Owner URL to the CONTROL database holding `wamn_run.gate_reports`.
    ///
    /// A separate URL because the report is a separate plane's fact: it is not
    /// in `catalog.wirings` and never was after wamn-0h0g.8.5.6. Pointing this
    /// at the project database refuses rather than passing — the relation is
    /// not there.
    #[arg(long)]
    pub control_database_url: String,

    /// Tenant claim carried by the authored wiring.
    #[arg(long)]
    pub tenant: String,

    /// Package identity the wiring is authored into.
    #[arg(long)]
    pub package_id: String,

    /// Exact package version whose component facts gate this wiring.
    #[arg(long)]
    pub package_version: String,

    /// The wiring document to submit; it carries its own id and version.
    #[arg(long)]
    pub wiring_document: PathBuf,
}

/// Author one gated wiring version and print its definition hash.
pub async fn run(args: AuthorWiringArgs) -> anyhow::Result<()> {
    let document = read_wiring_document(&args.wiring_document)?;
    let request = AuthorWiringRequest {
        tenant_id: &args.tenant,
        package_id: &args.package_id,
        package_version: &args.package_version,
        document: &document,
    };

    let (control, control_connection) = tokio_postgres::connect(&args.control_database_url, NoTls)
        .await
        .context("connect to the control store holding the gate reports")?;
    let control_task = tokio::spawn(control_connection);
    let opened = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to the authoring project environment");
    let (mut client, connection) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            control_task.abort();
            return Err(error);
        }
    };
    let connection_task = tokio::spawn(connection);
    let authored = author_in_transaction(&control, &mut client, &request).await;
    match authored {
        Ok(hash) => {
            drop(client);
            drop(control);
            connection_task
                .await
                .context("join the wiring authorship connection")?
                .context("drive the wiring authorship connection")?;
            control_task
                .await
                .context("join the gate-report connection")?
                .context("drive the gate-report connection")?;
            println!("{hash}");
            Ok(())
        }
        Err(error) => {
            connection_task.abort();
            control_task.abort();
            Err(error)
        }
    }
}

async fn author_in_transaction(
    control: &Client,
    client: &mut Client,
    request: &AuthorWiringRequest<'_>,
) -> anyhow::Result<DefinitionHash> {
    let transaction = client
        .transaction()
        .await
        .context("begin the wiring authorship")?;
    let hash = author_wiring(control, &transaction, request).await?;
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
    scope: &ComponentPackageScope,
    components: &[AdmittedComponent],
) -> Result<DefinitionHash, AuthorWiringError> {
    validate_wiring_compatibility(document, scope, components).map_err(|error| {
        AuthorWiringError::with_source(
            AuthorWiringErrorKind::Gate,
            format!(
                "wiring {:?} version {} is not compatible with package {}@{} component facts",
                document.wiring_id, document.version, scope.package_id, scope.package_version
            ),
            error,
        )
    })?;
    Ok(document.wiring_hash())
}

/// Read the control store's verdict for one wiring hash.
///
/// `None` means the control store holds NO report under this key. That is the
/// one answer a never-gated document, a gate-refused document, and a green
/// report over different bytes all produce, because the gate writes one row per
/// document it judges and keys it by that document's own hash.
async fn read_gate_verdict(
    control: &Client,
    tenant_id: &str,
    wiring_hash: &DefinitionHash,
) -> Result<Option<bool>, AuthorWiringError> {
    control
        .query_one(CLAIM_CONTROL_TENANT_SQL, &[&tenant_id])
        .await
        .map_err(|error| storage("claim the gate-report tenant", error))?;
    let hash = wiring_hash.as_str();
    let report = control
        .query_opt(SELECT_GATE_REPORT_SQL, &[&tenant_id, &hash])
        .await
        .map_err(|error| storage("read the wiring's gate report", error))?;
    Ok(report.map(|row| row.get(0)))
}

/// Require a GREEN verdict from the report read under this document's own hash.
///
/// The hash is the whole binding. This decides only what the control store
/// answered for that exact key, so a green report belonging to another document
/// cannot reach here as `Some(true)` — it is not under this key at all, and
/// arrives as the same `None` a never-gated document does.
fn require_green_report(
    verdict: Option<bool>,
    document: &WiringDocument,
    wiring_hash: &DefinitionHash,
) -> Result<(), AuthorWiringError> {
    match verdict {
        Some(true) => Ok(()),
        Some(false) => Err(AuthorWiringError::new(
            AuthorWiringErrorKind::Report,
            format!(
                "wiring {:?} version {} was gated and REFUSED at {}",
                document.wiring_id,
                document.version,
                wiring_hash.as_str()
            ),
        )),
        None => Err(AuthorWiringError::new(
            AuthorWiringErrorKind::Report,
            format!(
                "wiring {:?} version {} has no gate report at {}",
                document.wiring_id,
                document.version,
                wiring_hash.as_str()
            ),
        )),
    }
}

/// Gate and append one immutable wiring definition in the caller's transaction.
///
/// `control` is the CONTROL-database connection the gate report is read on. It
/// is a parameter and not an option: the read happens on every call, between the
/// compatibility gate and the INSERT, so no caller can author a wiring the
/// control store has not passed under that wiring's own hash.
pub async fn author_wiring(
    control: &Client,
    transaction: &Transaction<'_>,
    request: &AuthorWiringRequest<'_>,
) -> Result<DefinitionHash, AuthorWiringError> {
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&request.tenant_id])
        .await
        .map_err(|error| storage("claim the authoring tenant", error))?;

    let scope = ComponentPackageScope {
        tenant_id: request.tenant_id.to_string(),
        package_id: request.package_id.to_string(),
        package_version: request.package_version.to_string(),
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
    let verdict = read_gate_verdict(control, request.tenant_id, &wiring_hash).await?;
    require_green_report(verdict, request.document, &wiring_hash)?;

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
        &request.package_id,
        &request.package_version,
        &request.document.wiring_id,
        &version,
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

    /// Host command for the flattened argument surface under test.
    #[derive(Debug, clap::Parser)]
    struct AuthorProbe {
        #[command(flatten)]
        args: AuthorWiringArgs,
    }

    const COORDINATE: [&str; 10] = [
        "--database-url",
        "postgres://author.invalid/env",
        "--control-database-url",
        "postgres://author.invalid/control",
        "--tenant",
        "tenant-a",
        "--package-id",
        "orders",
        "--package-version",
        "3.0.0",
    ];

    fn parse(submission: &[&str]) -> Result<AuthorWiringArgs, clap::Error> {
        let mut argv = vec!["author-wiring"];
        argv.extend_from_slice(&COORDINATE);
        argv.extend_from_slice(submission);
        AuthorProbe::try_parse_from(argv).map(|probe| probe.args)
    }

    fn scope() -> ComponentPackageScope {
        ComponentPackageScope {
            tenant_id: "tenant-a".to_owned(),
            package_id: "orders".to_owned(),
            package_version: "3.0.0".to_owned(),
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
                    operation_dependency: None,
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
            operations: BTreeMap::from([(
                "call".to_owned(),
                wamn_catalog::AdmittedComponentOperation {
                    registered_operation: None,
                    dependencies: Vec::new(),
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                },
            )]),
            component_digest: format!("sha256:{}", "1".repeat(64)),
            imports: Vec::new(),
            imports_fingerprint: format!("sha256:{}", "6".repeat(64)),
            effects: Vec::new(),
        }
    }

    /// The document is the WHOLE submission, and argv adds nothing to it.
    ///
    /// The gate-report argument this used to require is gone (wamn-0h0g.8.5.6):
    /// the report keys on the wiring hash the document itself determines, so
    /// there is no report id left for a caller to supply or mis-supply. What
    /// argv still carries is WHERE to read that report — a database URL, not an
    /// identity — and it is required, so no invocation can omit the check.
    #[test]
    fn the_document_is_the_whole_artifact_and_argv_restates_nothing() {
        let complete =
            parse(&["--wiring-document", "wiring.json"]).expect("the submission surface parses");
        assert_eq!(complete.wiring_document, PathBuf::from("wiring.json"));
        assert_eq!(
            complete.control_database_url,
            "postgres://author.invalid/control"
        );

        // The control store is not optional: drop its URL and the verb cannot
        // be invoked at all, so there is no ungated authoring invocation.
        let mut without_control = vec!["author-wiring"];
        without_control.extend_from_slice(&COORDINATE[..2]);
        without_control.extend_from_slice(&COORDINATE[4..]);
        without_control.extend_from_slice(&["--wiring-document", "wiring.json"]);
        assert!(
            AuthorProbe::try_parse_from(without_control).is_err(),
            "authoring parsed with no control store to read the gate report from"
        );

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

    /// Only a GREEN verdict authorizes, and each refusal says which state it is.
    ///
    /// The absent arm is also the wrong-document arm: the read is keyed by the
    /// hash the gate produced, so a green report over other bytes is never
    /// offered to this decision as `Some(true)`. The test below pins that key,
    /// and `author_wiring_gate_report_live.rs` exercises it against a real
    /// report row in a real control database.
    #[test]
    fn only_a_green_report_for_this_document_authorizes_authorship() {
        let document = document();
        let hash = document.wiring_hash();

        require_green_report(Some(true), &document, &hash)
            .expect("a green report at the document's own hash authorizes it");

        let red = require_green_report(Some(false), &document, &hash)
            .expect_err("a report whose gate refused the document refuses authorship");
        assert_eq!(red.kind(), AuthorWiringErrorKind::Report);
        assert!(
            format!("{red}").starts_with(WIRING_AUTHORSHIP_REFUSAL),
            "refusal is unlabelled: {red}"
        );

        let absent = require_green_report(None, &document, &hash)
            .expect_err("an ungated document refuses authorship");
        assert_eq!(absent.kind(), AuthorWiringErrorKind::Report);
        assert_ne!(
            format!("{absent}"),
            format!("{red}"),
            "a red report and no report read as the same refusal"
        );
        assert!(format!("{absent}").contains(hash.as_str()));
    }

    /// The report is read under the gated hash, in the plane that holds it.
    #[test]
    fn the_report_is_read_under_the_hash_the_gate_produced() {
        assert!(SELECT_GATE_REPORT_SQL.contains("FROM wamn_run.gate_reports"));
        assert!(SELECT_GATE_REPORT_SQL.contains("WHERE tenant_id = $1 AND wiring_hash = $2"));
        // The control read is a READ. Authorship must not be able to mint the
        // permission it is asking for.
        for forbidden in ["INSERT", "UPDATE", "DELETE"] {
            assert!(
                !SELECT_GATE_REPORT_SQL.contains(forbidden),
                "the gate-report read carries {forbidden}"
            );
        }
        // The report relation is a CONTROL-plane fact: the project schema this
        // verb writes into has no such column or relation to check instead
        // (wamn-0h0g.8.5.6).
    }

    #[test]
    fn the_written_row_is_the_documents_own_identity() {
        for column in [
            "tenant_id",
            "package_id",
            "package_version",
            "wiring_id",
            "version",
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
