//! A structured SQL grammar for the `sql_lex` property tests.
//!
//! Each type here is an AST node with a `proptest` [`Arbitrary`] implementation
//! and a renderer. Rendering emits one lexical piece at a time, so a rendering
//! [`Style`] can vary keyword case, identifier quoting, and inert text
//! (comments, string bodies, dollar-quoted bodies) without changing the token
//! stream the lexer must see. That separation is the point: the AST states what
//! the authored SQL means, and the lexer must recover exactly that meaning from
//! any rendering of it.
//!
//! Nothing in this file depends on the test harness. The Phase 3 libFuzzer
//! target (D6b in `docs/poc/deterministic-testing-spec.md`) reuses it as its
//! input grammar by swapping the `Arbitrary` implementations below for
//! `arbitrary::Arbitrary` derives; the AST, the renderer, and the expectation
//! carry over unchanged.

use std::collections::{BTreeMap, BTreeSet};

use proptest::arbitrary::{Arbitrary, any};
use proptest::collection::{btree_set, vec};
use proptest::option;
use proptest::sample::select;
use proptest::strategy::{BoxedStrategy, Just, Strategy};

/// The relation catalog every generated statement is checked against.
///
/// `id` is shared so unqualified references stay ambiguous across relations,
/// and every other column belongs to exactly one relation.
pub(crate) static CATALOG: [(&str, &[&str]); 2] = [
    ("item", &["id", "quantity", "status"]),
    ("pallet", &["code", "id", "item_id"]),
];

/// Alias names that are neither keywords, relation names, nor column names.
static ALIASES: [&str; 3] = ["r", "t", "src"];

/// The output name a RETURNING item may be labelled with. Not a column of any
/// relation, so labelling one must not add authority.
static RETURNING_LABEL: &str = "out_a";

/// Row-lock strengths the admitted subset accepts.
static LOCK_STRENGTHS: [&[&str]; 4] = [
    &["update"],
    &["share"],
    &["no", "key", "update"],
    &["key", "share"],
];

/// Statement keywords the manifest never grants authority for.
static UNSUPPORTED_EFFECTS: [&str; 9] = [
    "alter", "call", "copy", "create", "drop", "grant", "merge", "revoke", "truncate",
];

/// Line-comment bodies. No newline may appear, or the comment ends early.
static LINE_BODIES: [&str; 4] = [
    " drop table item",
    " delete from item returning *",
    " sched.item ' \" $ /* */ ",
    "",
];

/// Block-comment bodies. No `/` or `*` may appear, or the nesting shifts.
static BLOCK_BODIES: [&str; 4] = [" truncate item ", " sched.item ", " ' \" $1 , ) ", ""];

/// Single-quoted string bodies. No `'` may appear unescaped.
static STRING_BODIES: [&str; 4] = [" grant all on item ", " sched.item ", " -- */ \" $ ", ""];

/// Dollar-quoted bodies. The `$q$` delimiter may not appear.
static DOLLAR_BODIES: [&str; 4] = [
    " revoke select on item ",
    " sched.item ",
    " -- /* ' \" ",
    "",
];

/// The catalog in the shape `relation_access` takes.
pub(crate) fn catalog() -> BTreeMap<String, BTreeSet<String>> {
    CATALOG
        .iter()
        .map(|(relation, fields)| {
            (
                (*relation).to_owned(),
                fields.iter().map(|field| (*field).to_owned()).collect(),
            )
        })
        .collect()
}

/// One lexical unit. Rendering separates every piece with whitespace, so no
/// piece can merge into its neighbour whatever the style asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Piece {
    Ident(&'static str),
    Dot,
    LeftParen,
    RightParen,
    Comma,
    Equals,
    Star,
    Param(usize),
}

/// Text that carries no token: the lexer must treat all four kinds as absent.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Inert {
    LineComment(&'static str),
    BlockComment(&'static str),
    StringLiteral(&'static str),
    DollarQuoted(&'static str),
}

/// How a piece list is written out. None of it may change the derived access.
#[derive(Debug, Clone)]
pub(crate) struct Style {
    pub(crate) uppercase: bool,
    pub(crate) quote_identifiers: bool,
    /// Inert text to place before the piece at each index, and one trailing
    /// slot at `pieces.len()`.
    pub(crate) inert: Vec<Option<Inert>>,
}

impl Style {
    /// Lower case, unquoted, no inert text: the reference rendering.
    pub(crate) const fn canonical() -> Self {
        Self {
            uppercase: false,
            quote_identifiers: false,
            inert: Vec::new(),
        }
    }
}

/// The access a statement declares by construction, before the lexer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpectedAccess {
    pub(crate) relation: &'static str,
    pub(crate) select_fields: BTreeSet<String>,
    pub(crate) insert_fields: BTreeSet<String>,
    pub(crate) update_fields: BTreeSet<String>,
    pub(crate) lock: bool,
}

pub(crate) fn render(pieces: &[Piece], style: &Style) -> String {
    let mut sql = String::new();
    for (index, piece) in pieces.iter().enumerate() {
        push_inert(style.inert.get(index), &mut sql);
        push_piece(*piece, style, &mut sql);
        sql.push(' ');
    }
    push_inert(style.inert.get(pieces.len()), &mut sql);
    sql
}

fn push_inert(slot: Option<&Option<Inert>>, sql: &mut String) {
    let Some(Some(inert)) = slot else {
        return;
    };
    match *inert {
        Inert::LineComment(body) => {
            sql.push_str("--");
            sql.push_str(body);
            sql.push('\n');
        }
        Inert::BlockComment(body) => {
            sql.push_str("/*");
            sql.push_str(body);
            sql.push_str("*/");
        }
        Inert::StringLiteral(body) => {
            sql.push('\'');
            sql.push_str(body);
            sql.push('\'');
        }
        Inert::DollarQuoted(body) => {
            sql.push_str("$q$");
            sql.push_str(body);
            sql.push_str("$q$");
        }
    }
    sql.push(' ');
}

fn push_piece(piece: Piece, style: &Style, sql: &mut String) {
    match piece {
        // A quoted identifier keeps its case, so quoting an already-lower-case
        // name must be equivalent to writing it bare.
        Piece::Ident(value) if style.quote_identifiers => {
            sql.push('"');
            sql.push_str(value);
            sql.push('"');
        }
        Piece::Ident(value) if style.uppercase => sql.push_str(&value.to_ascii_uppercase()),
        Piece::Ident(value) => sql.push_str(value),
        Piece::Dot => sql.push('.'),
        Piece::LeftParen => sql.push('('),
        Piece::RightParen => sql.push(')'),
        Piece::Comma => sql.push(','),
        Piece::Equals => sql.push('='),
        Piece::Star => sql.push('*'),
        Piece::Param(ordinal) => {
            sql.push('$');
            sql.push_str(&ordinal.to_string());
        }
    }
}

/// A statement inside the admitted subset. Every one of these must parse.
#[derive(Debug, Clone)]
pub(crate) enum Statement {
    Select(SelectStatement),
    Insert(InsertStatement),
    Update(UpdateStatement),
}

#[derive(Debug, Clone)]
pub(crate) struct SelectStatement {
    relation: usize,
    alias: Option<&'static str>,
    keyword_as: bool,
    projection: Vec<usize>,
    qualify: bool,
    filter: Option<usize>,
    lock: Option<Lock>,
}

#[derive(Debug, Clone)]
pub(crate) struct InsertStatement {
    relation: usize,
    columns: Vec<usize>,
    returning: Vec<usize>,
    label_returning: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct UpdateStatement {
    relation: usize,
    assignments: Vec<usize>,
    filter: Option<usize>,
    qualify_filter: bool,
    returning: Vec<usize>,
    label_returning: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Lock {
    strength: &'static [&'static str],
    of: bool,
}

impl Statement {
    pub(crate) fn pieces(&self) -> Vec<Piece> {
        match self {
            Self::Select(statement) => statement.pieces(),
            Self::Insert(statement) => statement.pieces(),
            Self::Update(statement) => statement.pieces(),
        }
    }

    pub(crate) fn expected(&self) -> ExpectedAccess {
        match self {
            Self::Select(statement) => statement.expected(),
            Self::Insert(statement) => statement.expected(),
            Self::Update(statement) => statement.expected(),
        }
    }
}

impl SelectStatement {
    fn relation_name(&self) -> &'static str {
        CATALOG[self.relation].0
    }

    fn column(&self, index: usize) -> &'static str {
        CATALOG[self.relation].1[index]
    }

    /// The name a qualified reference uses: the alias when there is one.
    fn scope(&self) -> &'static str {
        self.alias.unwrap_or_else(|| self.relation_name())
    }

    fn pieces(&self) -> Vec<Piece> {
        let mut pieces = vec![Piece::Ident("select")];
        for (position, column) in self.projection.iter().enumerate() {
            if position > 0 {
                pieces.push(Piece::Comma);
            }
            push_reference(
                &mut pieces,
                self.qualify.then(|| self.scope()),
                self.column(*column),
            );
        }
        pieces.push(Piece::Ident("from"));
        pieces.push(Piece::Ident(self.relation_name()));
        if let Some(alias) = self.alias {
            if self.keyword_as {
                pieces.push(Piece::Ident("as"));
            }
            pieces.push(Piece::Ident(alias));
        }
        if let Some(filter) = self.filter {
            pieces.push(Piece::Ident("where"));
            push_reference(
                &mut pieces,
                self.qualify.then(|| self.scope()),
                self.column(filter),
            );
            pieces.push(Piece::Equals);
            pieces.push(Piece::Param(1));
        }
        if let Some(lock) = &self.lock {
            pieces.push(Piece::Ident("for"));
            pieces.extend(lock.strength.iter().map(|word| Piece::Ident(word)));
            if lock.of {
                pieces.push(Piece::Ident("of"));
                pieces.push(Piece::Ident(self.scope()));
            }
        }
        pieces
    }

    fn expected(&self) -> ExpectedAccess {
        let mut select_fields = self
            .projection
            .iter()
            .map(|column| self.column(*column).to_owned())
            .collect::<BTreeSet<_>>();
        if let Some(filter) = self.filter {
            select_fields.insert(self.column(filter).to_owned());
        }
        ExpectedAccess {
            relation: self.relation_name(),
            select_fields,
            insert_fields: BTreeSet::new(),
            update_fields: BTreeSet::new(),
            lock: self.lock.is_some(),
        }
    }
}

impl InsertStatement {
    fn relation_name(&self) -> &'static str {
        CATALOG[self.relation].0
    }

    fn column(&self, index: usize) -> &'static str {
        CATALOG[self.relation].1[index]
    }

    fn pieces(&self) -> Vec<Piece> {
        let mut pieces = vec![
            Piece::Ident("insert"),
            Piece::Ident("into"),
            Piece::Ident(self.relation_name()),
            Piece::LeftParen,
        ];
        push_column_list(&mut pieces, self.columns.iter().map(|c| self.column(*c)));
        pieces.push(Piece::RightParen);
        pieces.push(Piece::Ident("values"));
        pieces.push(Piece::LeftParen);
        for position in 0..self.columns.len() {
            if position > 0 {
                pieces.push(Piece::Comma);
            }
            pieces.push(Piece::Param(position + 1));
        }
        pieces.push(Piece::RightParen);
        push_returning(
            &mut pieces,
            &self
                .returning
                .iter()
                .map(|c| self.column(*c))
                .collect::<Vec<_>>(),
            self.label_returning,
        );
        pieces
    }

    fn expected(&self) -> ExpectedAccess {
        ExpectedAccess {
            relation: self.relation_name(),
            select_fields: self
                .returning
                .iter()
                .map(|column| self.column(*column).to_owned())
                .collect(),
            insert_fields: self
                .columns
                .iter()
                .map(|column| self.column(*column).to_owned())
                .collect(),
            update_fields: BTreeSet::new(),
            lock: false,
        }
    }
}

impl UpdateStatement {
    fn relation_name(&self) -> &'static str {
        CATALOG[self.relation].0
    }

    fn column(&self, index: usize) -> &'static str {
        CATALOG[self.relation].1[index]
    }

    fn pieces(&self) -> Vec<Piece> {
        let mut pieces = vec![
            Piece::Ident("update"),
            Piece::Ident(self.relation_name()),
            Piece::Ident("set"),
        ];
        for (position, column) in self.assignments.iter().enumerate() {
            if position > 0 {
                pieces.push(Piece::Comma);
            }
            pieces.push(Piece::Ident(self.column(*column)));
            pieces.push(Piece::Equals);
            pieces.push(Piece::Param(position + 1));
        }
        if let Some(filter) = self.filter {
            pieces.push(Piece::Ident("where"));
            push_reference(
                &mut pieces,
                self.qualify_filter.then(|| self.relation_name()),
                self.column(filter),
            );
            pieces.push(Piece::Equals);
            pieces.push(Piece::Param(self.assignments.len() + 1));
        }
        push_returning(
            &mut pieces,
            &self
                .returning
                .iter()
                .map(|c| self.column(*c))
                .collect::<Vec<_>>(),
            self.label_returning,
        );
        pieces
    }

    fn expected(&self) -> ExpectedAccess {
        let mut select_fields = self
            .returning
            .iter()
            .map(|column| self.column(*column).to_owned())
            .collect::<BTreeSet<_>>();
        if let Some(filter) = self.filter {
            select_fields.insert(self.column(filter).to_owned());
        }
        ExpectedAccess {
            relation: self.relation_name(),
            select_fields,
            insert_fields: BTreeSet::new(),
            update_fields: self
                .assignments
                .iter()
                .map(|column| self.column(*column).to_owned())
                .collect(),
            lock: false,
        }
    }
}

fn push_reference(pieces: &mut Vec<Piece>, scope: Option<&'static str>, column: &'static str) {
    if let Some(scope) = scope {
        pieces.push(Piece::Ident(scope));
        pieces.push(Piece::Dot);
    }
    pieces.push(Piece::Ident(column));
}

fn push_column_list(pieces: &mut Vec<Piece>, columns: impl Iterator<Item = &'static str>) {
    for (position, column) in columns.enumerate() {
        if position > 0 {
            pieces.push(Piece::Comma);
        }
        pieces.push(Piece::Ident(column));
    }
}

/// A RETURNING list, optionally labelling its first item. A label is neither a
/// column nor a keyword, so it must contribute nothing to the derived access.
fn push_returning(pieces: &mut Vec<Piece>, columns: &[&'static str], label: bool) {
    if columns.is_empty() {
        return;
    }
    pieces.push(Piece::Ident("returning"));
    for (position, column) in columns.iter().enumerate() {
        if position > 0 {
            pieces.push(Piece::Comma);
        }
        pieces.push(Piece::Ident(column));
        if position == 0 && label {
            pieces.push(Piece::Ident("as"));
            pieces.push(Piece::Ident(RETURNING_LABEL));
        }
    }
}

/// A statement the lexer must refuse. Every variant is a distinct refusal path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Forbidden {
    DeleteFrom,
    DeleteInsideCte,
    UnsupportedEffect(&'static str),
    SelectStar,
    ReturningStar,
    TupleAssignment,
    UnmatchedOpenParen,
    UnmatchedCloseParen,
    UndeclaredInsertRelation,
    UnknownInsertColumn,
    InsertWithoutColumnList,
    UpdateWithoutSet,
    UndeclaredUpdateRelation,
    LockOfUndeclaredRelation,
}

impl Forbidden {
    /// Every refusal path, committed as cases rather than sampled.
    pub(crate) fn all() -> Vec<Self> {
        let mut cases = vec![
            Self::DeleteFrom,
            Self::DeleteInsideCte,
            Self::SelectStar,
            Self::ReturningStar,
            Self::TupleAssignment,
            Self::UnmatchedOpenParen,
            Self::UnmatchedCloseParen,
            Self::UndeclaredInsertRelation,
            Self::UnknownInsertColumn,
            Self::InsertWithoutColumnList,
            Self::UpdateWithoutSet,
            Self::UndeclaredUpdateRelation,
            Self::LockOfUndeclaredRelation,
        ];
        cases.extend(
            UNSUPPORTED_EFFECTS
                .iter()
                .map(|effect| Self::UnsupportedEffect(effect)),
        );
        cases
    }

    pub(crate) fn pieces(self) -> Vec<Piece> {
        match self {
            Self::DeleteFrom => idents(&["delete", "from", "item"]),
            Self::DeleteInsideCte => {
                let mut pieces = idents(&["with", "removed", "as"]);
                pieces.push(Piece::LeftParen);
                pieces.extend(idents(&["delete", "from", "item", "returning", "id"]));
                pieces.push(Piece::RightParen);
                pieces.extend(idents(&["select", "removed"]));
                pieces.push(Piece::Dot);
                pieces.extend(idents(&["id", "from", "removed"]));
                pieces
            }
            Self::UnsupportedEffect(effect) => vec![Piece::Ident(effect), Piece::Ident("item")],
            Self::SelectStar => {
                let mut pieces = idents(&["select"]);
                pieces.push(Piece::Star);
                pieces.extend(idents(&["from", "item"]));
                pieces
            }
            Self::ReturningStar => {
                let mut pieces = insert_head("item", "id");
                pieces.extend(idents(&["returning"]));
                pieces.push(Piece::Star);
                pieces
            }
            Self::TupleAssignment => {
                let mut pieces = idents(&["update", "item", "set"]);
                pieces.push(Piece::LeftParen);
                pieces.push(Piece::Ident("id"));
                pieces.push(Piece::Comma);
                pieces.push(Piece::Ident("status"));
                pieces.push(Piece::RightParen);
                pieces.push(Piece::Equals);
                pieces.push(Piece::LeftParen);
                pieces.push(Piece::Param(1));
                pieces.push(Piece::Comma);
                pieces.push(Piece::Param(2));
                pieces.push(Piece::RightParen);
                pieces
            }
            Self::UnmatchedOpenParen => {
                let mut pieces = idents(&["select", "id", "from", "item", "where"]);
                pieces.push(Piece::LeftParen);
                pieces.push(Piece::Ident("status"));
                pieces.push(Piece::Equals);
                pieces.push(Piece::Param(1));
                pieces
            }
            Self::UnmatchedCloseParen => {
                let mut pieces = idents(&["select", "id", "from", "item"]);
                pieces.push(Piece::RightParen);
                pieces
            }
            Self::UndeclaredInsertRelation => insert_head("ghost", "id"),
            Self::UnknownInsertColumn => insert_head("item", "ghost"),
            Self::InsertWithoutColumnList => {
                let mut pieces = idents(&["insert", "into", "item", "values"]);
                pieces.push(Piece::LeftParen);
                pieces.push(Piece::Param(1));
                pieces.push(Piece::RightParen);
                pieces
            }
            Self::UpdateWithoutSet => {
                let mut pieces = idents(&["update", "item", "where", "item"]);
                pieces.push(Piece::Dot);
                pieces.push(Piece::Ident("id"));
                pieces.push(Piece::Equals);
                pieces.push(Piece::Param(1));
                pieces
            }
            Self::UndeclaredUpdateRelation => {
                let mut pieces = idents(&["update", "ghost", "set", "status"]);
                pieces.push(Piece::Equals);
                pieces.push(Piece::Param(1));
                pieces
            }
            Self::LockOfUndeclaredRelation => idents(&[
                "select", "id", "from", "item", "for", "update", "of", "ghost",
            ]),
        }
    }
}

fn idents(words: &[&'static str]) -> Vec<Piece> {
    words.iter().map(|word| Piece::Ident(word)).collect()
}

fn insert_head(relation: &'static str, column: &'static str) -> Vec<Piece> {
    vec![
        Piece::Ident("insert"),
        Piece::Ident("into"),
        Piece::Ident(relation),
        Piece::LeftParen,
        Piece::Ident(column),
        Piece::RightParen,
        Piece::Ident("values"),
        Piece::LeftParen,
        Piece::Param(1),
        Piece::RightParen,
    ]
}

impl Arbitrary for Inert {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
        proptest::prop_oneof![
            select(LINE_BODIES.to_vec()).prop_map(Inert::LineComment),
            select(BLOCK_BODIES.to_vec()).prop_map(Inert::BlockComment),
            select(STRING_BODIES.to_vec()).prop_map(Inert::StringLiteral),
            select(DOLLAR_BODIES.to_vec()).prop_map(Inert::DollarQuoted),
        ]
        .boxed()
    }
}

impl Arbitrary for Style {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
        (
            any::<bool>(),
            any::<bool>(),
            vec(option::of(any::<Inert>()), 0..48),
        )
            .prop_map(|(uppercase, quote_identifiers, inert)| Self {
                uppercase,
                quote_identifiers,
                inert,
            })
            .boxed()
    }
}

impl Arbitrary for Lock {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
        (select(LOCK_STRENGTHS.to_vec()), any::<bool>())
            .prop_map(|(strength, of)| Self { strength, of })
            .boxed()
    }
}

impl Arbitrary for Statement {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
        proptest::prop_oneof![
            any::<SelectStatement>().prop_map(Statement::Select),
            any::<InsertStatement>().prop_map(Statement::Insert),
            any::<UpdateStatement>().prop_map(Statement::Update),
        ]
        .boxed()
    }
}

impl Arbitrary for SelectStatement {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
        (0..CATALOG.len())
            .prop_flat_map(|relation| {
                let width = CATALOG[relation].1.len();
                (
                    Just(relation),
                    option::of(select(ALIASES.to_vec())),
                    any::<bool>(),
                    btree_set(0..width, 1..=width),
                    any::<bool>(),
                    option::of(0..width),
                    option::of(any::<Lock>()),
                )
            })
            .prop_map(
                |(relation, alias, keyword_as, projection, qualify, filter, lock)| Self {
                    relation,
                    alias,
                    keyword_as,
                    projection: projection.into_iter().collect(),
                    qualify,
                    filter,
                    lock,
                },
            )
            .boxed()
    }
}

impl Arbitrary for InsertStatement {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
        (0..CATALOG.len())
            .prop_flat_map(|relation| {
                let width = CATALOG[relation].1.len();
                (
                    Just(relation),
                    btree_set(0..width, 1..=width),
                    btree_set(0..width, 0..=width),
                    any::<bool>(),
                )
            })
            .prop_map(|(relation, columns, returning, label_returning)| Self {
                relation,
                columns: columns.into_iter().collect(),
                returning: returning.into_iter().collect(),
                label_returning,
            })
            .boxed()
    }
}

impl Arbitrary for UpdateStatement {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
        (0..CATALOG.len())
            .prop_flat_map(|relation| {
                let width = CATALOG[relation].1.len();
                (
                    Just(relation),
                    btree_set(0..width, 1..=width),
                    option::of(0..width),
                    any::<bool>(),
                    btree_set(0..width, 0..=width),
                    any::<bool>(),
                )
            })
            .prop_map(
                |(relation, assignments, filter, qualify_filter, returning, label_returning)| {
                    Self {
                        relation,
                        assignments: assignments.into_iter().collect(),
                        filter,
                        qualify_filter,
                        returning: returning.into_iter().collect(),
                        label_returning,
                    }
                },
            )
            .boxed()
    }
}

impl Arbitrary for Forbidden {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
        select(Self::all()).boxed()
    }
}
