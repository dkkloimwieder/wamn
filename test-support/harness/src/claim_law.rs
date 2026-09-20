//! Tests claim replay against the SQL named by an operation contract.
//!
//! The runner owns the two claim-law cases. The generated operation contract
//! supplies the claim, finalization, and replay statements with their binds.
//! Both cases execute the actual SQL against PostgreSQL and observe writes.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::Value;
use tokio_postgres::types::{FromSql, ToSql, Type};
use tokio_postgres::{Client, Row, Transaction};

/// The only law this runner knows how to check.
pub const LAW: &str = "command-identity-from-claim";

/// PostgreSQL assigns a transaction id the first time a transaction writes a
/// row. A transaction that only reads never gets one, and this function
/// answers NULL for it. The runner asks inside the transaction, so the answer
/// covers every table rather than a list someone chose, and it still fails
/// when an insert is cancelled by a later delete. The first call asks the same
/// question and must get an id back, so a detector that always answered NULL
/// would fail there first.
const ASSIGNED_TRANSACTION_ID: &str = "SELECT pg_current_xact_id_if_assigned()::text AS assigned";

/// One column value exactly as the server sent it.
///
/// The runner compares results without knowing any column's Rust type, so a
/// new operation needs no new code here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cell {
    kind: Type,
    bytes: Option<Vec<u8>>,
}

impl Cell {
    /// The raw value, or `None` for SQL NULL.
    pub fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }

    /// The type the server sent the value as.
    pub fn kind(&self) -> &Type {
        &self.kind
    }
}

impl<'a> FromSql<'a> for Cell {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(Self {
            kind: ty.clone(),
            bytes: Some(raw.to_vec()),
        })
    }

    fn from_sql_null(ty: &Type) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(Self {
            kind: ty.clone(),
            bytes: None,
        })
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

/// One statement the operation contract names.
#[derive(Clone, Debug)]
pub struct Statement {
    /// The name the cases use.
    pub name: String,
    /// Where the SQL lives, relative to the package root.
    pub path: PathBuf,
    /// The SQL the runtime executes.
    pub sql: String,
    /// The binds the contract declares, in order.
    pub binds: Vec<String>,
}

/// One claim-law case.
#[derive(Clone, Debug)]
pub struct Case {
    /// The case id.
    pub id: String,
    /// The request that the case repeats.
    pub given: String,
    first_call: Vec<String>,
    second_call: Vec<String>,
    canonical_command: String,
    claim: String,
    writes: String,
    result: Option<String>,
    refusal: Option<String>,
}

/// An operation contract and its claim-law cases.
#[derive(Clone, Debug)]
pub struct ClaimContract {
    /// The operation the cases belong to.
    pub operation: String,
    /// The law the cases test.
    pub law: String,
    /// The replay and changed-request cases.
    pub cases: Vec<Case>,
    statements: BTreeMap<String, Statement>,
}

/// What one case measured.
#[derive(Clone, Debug)]
pub struct CaseReport {
    /// The case id.
    pub id: String,
    /// The id the claim statement generated on the first call.
    pub claim_identity: BTreeMap<String, Cell>,
    /// The transaction id the first call was given. The first call writes, so
    /// this is always present, and it shows that the write detector works.
    pub first_call_transaction_id: String,
    /// The refusal the second call produced, when the case declares one.
    pub refusal: Option<String>,
}

/// One bind value, ready to send as a query parameter.
pub type BindValue = Box<dyn ToSql + Sync>;

/// Supplies the bind values the emitted statements take.
///
/// The runner never asks for `canonical_command`: the law owns that value and
/// varies it between the two cases.
pub trait CommandFixture {
    /// One bind value. `claim` is the row the claim statement returned in this
    /// call, and is `None` before that statement ran.
    ///
    /// # Errors
    ///
    /// When the fixture has no value for the named bind.
    fn bind(&self, statement: &str, bind: &str, claim: Option<&Row>) -> Result<BindValue>;
}

/// Read a claim-bearing operation and every statement it names.
///
/// `operation_path` is relative to `package_root`.
///
/// # Errors
///
/// Returns an error for a missing or malformed contract, SQL file, or claim.
pub fn load(package_root: &Path, operation_path: &Path) -> Result<ClaimContract> {
    let operation_document = read_json(&package_root.join(operation_path))?;
    ensure!(
        text(&operation_document, "idempotent_by")? == "claim",
        "the operation must use claim idempotence"
    );
    let claim = operation_document
        .get("claim")
        .context("the operation carries a claim")?;
    let first_call = vec![text(claim, "claim")?, text(claim, "finalize")?];
    let second_call = vec![text(claim, "claim")?, text(claim, "replay")?];

    let mut statements = BTreeMap::new();
    for entry in array(&operation_document, "statements")? {
        let name = text(entry, "name")?;
        let path = PathBuf::from(text(entry, "path")?);
        let sql = fs::read_to_string(package_root.join(&path))
            .with_context(|| format!("read the emitted SQL at {}", path.display()))?;
        let binds = array(entry, "binds")?
            .iter()
            .map(|bind| text(bind, "name"))
            .collect::<Result<Vec<_>>>()?;
        statements.insert(
            name.clone(),
            Statement {
                name,
                path,
                sql,
                binds,
            },
        );
    }

    let cases = vec![
        Case {
            id: "replay_returns_the_immutable_original".to_owned(),
            given: "the same idempotency_key with the same canonical_command".to_owned(),
            first_call: first_call.clone(),
            second_call: second_call.clone(),
            canonical_command: "equal".to_owned(),
            claim: "no_row".to_owned(),
            writes: "none".to_owned(),
            result: Some("identical_to_the_first_call".to_owned()),
            refusal: None,
        },
        Case {
            id: "changed_request_under_a_live_key_refuses".to_owned(),
            given: "the same idempotency_key with a changed canonical_command".to_owned(),
            first_call,
            second_call,
            canonical_command: "differs".to_owned(),
            claim: "no_row".to_owned(),
            writes: "none".to_owned(),
            result: None,
            refusal: Some("idempotency_conflict".to_owned()),
        },
    ];

    Ok(ClaimContract {
        operation: text(&operation_document, "operation")?,
        law: LAW.to_owned(),
        cases,
        statements,
    })
}

impl ClaimContract {
    /// The case with this id.
    ///
    /// # Errors
    ///
    /// When the runner defines no such case.
    pub fn case(&self, id: &str) -> Result<&Case> {
        self.cases
            .iter()
            .find(|case| case.id == id)
            .with_context(|| format!("the runner defines no case {id}"))
    }

    /// Replace one exact fragment of one statement's SQL.
    ///
    /// This makes a mutant. The runner refuses a fragment that is absent or
    /// that appears more than once, so a mutant is always the single change it
    /// claims to be.
    ///
    /// # Errors
    ///
    /// When the statement is unknown, or the fragment does not appear exactly
    /// once in it.
    pub fn mutate(&mut self, statement: &str, from: &str, to: &str) -> Result<()> {
        let target = self
            .statements
            .get_mut(statement)
            .with_context(|| format!("the operation contract names no statement {statement}"))?;
        let hits = target.sql.matches(from).count();
        ensure!(
            hits == 1,
            "the mutant fragment appears {hits} times in {}, and a mutant must be one change",
            target.path.display()
        );
        target.sql = target.sql.replace(from, to);
        Ok(())
    }

    /// Run one claim-law case against a live database.
    ///
    /// The caller owns the schema and the fixture rows. `original_command` and
    /// `changed_command` are the canonical command bytes for the same
    /// idempotency key; the case decides which one the second call sends.
    ///
    /// # Errors
    ///
    /// When the database refuses a statement, or when the case expectation
    /// does not hold.
    pub async fn run_case(
        &self,
        client: &mut Client,
        case: &Case,
        fixture: &dyn CommandFixture,
        original_command: &[u8],
        changed_command: &[u8],
    ) -> Result<CaseReport> {
        ensure!(
            original_command != changed_command,
            "the changed request must differ from the original"
        );
        let claim_name = claim_statement(case)?;
        let replay_name = replay_statement(case, &claim_name)?;

        // FIRST CALL. This is the call the law makes immutable.
        let transaction = client.transaction().await.context("begin the first call")?;
        let first = self
            .run_call(
                &transaction,
                &case.first_call,
                &claim_name,
                fixture,
                original_command,
            )
            .await?;
        let first_call_transaction_id = first
            .transaction_id
            .clone()
            .context("the first call wrote, so PostgreSQL must have assigned it an id")?;
        transaction
            .commit()
            .await
            .context("commit the first call")?;

        let claim_identity = first
            .rows
            .get(&claim_name)
            .and_then(|rows| rows.first())
            .cloned()
            .context("the claim statement returned no row on the first call")?;

        // The original result, read back through the emitted replay statement
        // after the first call committed.
        let original_result = self
            .read_one(client, &replay_name, fixture, original_command)
            .await?;
        for (column, value) in &claim_identity {
            ensure!(
                original_result.get(column) == Some(value),
                "the durable row lost the id the claim generated for {column}"
            );
        }

        // SECOND CALL. Same key. The case decides whether the request changed.
        let sent = if case.canonical_command == "differs" {
            changed_command
        } else {
            original_command
        };
        let transaction = client
            .transaction()
            .await
            .context("begin the second call")?;
        let second = self
            .run_call(&transaction, &case.second_call, &claim_name, fixture, sent)
            .await?;
        if case.writes == "none" {
            ensure!(
                second.transaction_id.is_none(),
                "the second call wrote: PostgreSQL gave it transaction id {:?}",
                second.transaction_id
            );
        }
        if case.claim == "no_row" {
            let claimed = second.rows.get(&claim_name).map_or(0, Vec::len);
            ensure!(
                claimed == 0,
                "the claim statement returned {claimed} rows under a live key, and the case \
                 expects none"
            );
        }
        transaction
            .commit()
            .await
            .context("commit the second call")?;

        let replay = second
            .rows
            .get(&replay_name)
            .and_then(|rows| rows.first())
            .context("the replay statement returned no row under a live key")?;
        let stored = replay
            .get("canonical_command")
            .and_then(Cell::bytes)
            .context("the replay row carries no canonical_command")?;

        let refusal = if stored == sent {
            None
        } else {
            Some("idempotency_conflict".to_owned())
        };
        match (&case.refusal, &refusal) {
            (Some(expected), Some(actual)) => ensure!(
                expected == actual,
                "the case expects {expected} and the second call produced {actual}"
            ),
            (Some(expected), None) => bail!("the case expects {expected}, and it replayed"),
            (None, Some(actual)) => bail!("the second call refused with {actual} unexpectedly"),
            (None, None) => {}
        }
        if case.result.as_deref() == Some("identical_to_the_first_call") {
            ensure!(
                replay == &original_result,
                "the replay returned a different result from the first call"
            );
        }

        // Zero writes, witnessed a second time: the durable row is unchanged
        // after the second call committed. The transaction id above is the
        // stronger witness, because it covers tables this row cannot see.
        let after = self
            .read_one(client, &replay_name, fixture, original_command)
            .await?;
        ensure!(
            after == original_result,
            "the second call changed the durable row"
        );

        Ok(CaseReport {
            id: case.id.clone(),
            claim_identity,
            first_call_transaction_id,
            refusal,
        })
    }

    async fn run_call(
        &self,
        transaction: &Transaction<'_>,
        statements: &[String],
        claim_name: &str,
        fixture: &dyn CommandFixture,
        command: &[u8],
    ) -> Result<CallObservation> {
        let mut rows = BTreeMap::new();
        let mut claim: Option<Row> = None;
        for name in statements {
            let statement = self.statement(name)?;
            let values = bind_values(statement, fixture, claim.as_ref(), command)?;
            let borrowed = borrow(&values);
            let returned = transaction
                .query(&statement.sql, &borrowed)
                .await
                .with_context(|| format!("run the emitted {}", statement.path.display()))?;
            if name == claim_name {
                claim = returned.first().cloned();
            }
            rows.insert(
                name.clone(),
                returned.iter().map(cells).collect::<Result<Vec<_>>>()?,
            );
        }
        let assigned = transaction
            .query_one(ASSIGNED_TRANSACTION_ID, &[])
            .await
            .context("ask PostgreSQL whether this transaction was given a transaction id")?;
        Ok(CallObservation {
            rows,
            transaction_id: assigned.try_get::<_, Option<String>>("assigned")?,
        })
    }

    async fn read_one(
        &self,
        client: &Client,
        name: &str,
        fixture: &dyn CommandFixture,
        command: &[u8],
    ) -> Result<BTreeMap<String, Cell>> {
        let statement = self.statement(name)?;
        let values = bind_values(statement, fixture, None, command)?;
        let borrowed = borrow(&values);
        let row = client
            .query_one(&statement.sql, &borrowed)
            .await
            .with_context(|| format!("read the durable row with {}", statement.path.display()))?;
        cells(&row)
    }

    fn statement(&self, name: &str) -> Result<&Statement> {
        self.statements
            .get(name)
            .with_context(|| format!("the operation contract names no statement {name}"))
    }
}

/// The values one statement binds, in the order the operation contract names
/// them. The canonical command is the runner's, so a fixture cannot vary it.
fn bind_values(
    statement: &Statement,
    fixture: &dyn CommandFixture,
    claim: Option<&Row>,
    command: &[u8],
) -> Result<Vec<BindValue>> {
    statement
        .binds
        .iter()
        .map(|bind| -> Result<BindValue> {
            if bind == "canonical_command" {
                return Ok(Box::new(command.to_vec()));
            }
            fixture.bind(&statement.name, bind, claim)
        })
        .collect()
}

#[derive(Debug)]
struct CallObservation {
    rows: BTreeMap<String, Vec<BTreeMap<String, Cell>>>,
    transaction_id: Option<String>,
}

/// The query parameter slice `tokio_postgres` takes, over owned bind values.
fn borrow(values: &[BindValue]) -> Vec<&(dyn ToSql + Sync)> {
    values.iter().map(AsRef::as_ref).collect()
}

/// The claim statement is the one both calls run.
fn claim_statement(case: &Case) -> Result<String> {
    let mut shared = case
        .first_call
        .iter()
        .filter(|name| case.second_call.contains(name));
    let claim = shared
        .next()
        .context("no statement runs in both calls, so no statement holds the claim")?;
    ensure!(
        shared.next().is_none(),
        "more than one statement runs in both calls, so the claim is ambiguous"
    );
    Ok(claim.clone())
}

/// The replay read is the second call's other statement.
fn replay_statement(case: &Case, claim: &str) -> Result<String> {
    let mut rest = case
        .second_call
        .iter()
        .filter(|name| name.as_str() != claim);
    let replay = rest
        .next()
        .context("the second call reads no durable result")?;
    ensure!(
        rest.next().is_none(),
        "the second call reads the durable result with more than one statement"
    );
    Ok(replay.clone())
}

fn cells(row: &Row) -> Result<BTreeMap<String, Cell>> {
    let mut map = BTreeMap::new();
    for (index, column) in row.columns().iter().enumerate() {
        map.insert(column.name().to_owned(), row.try_get::<_, Cell>(index)?);
    }
    Ok(map)
}

fn read_json(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("{} is JSON", path.display()))
}

fn text(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("the emitted artifact carries a string at {key}"))
}

fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value]> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .with_context(|| format!("the emitted artifact carries an array at {key}"))
}
