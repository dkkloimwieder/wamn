//! Every guest-invoked host-plugin effect opens a span, from one constructor.
//!
//! # What this proves, and why it is a source proof
//!
//! `wamn-0h0g.24.3` put a span over every effect this host performs for a guest:
//! a DB call, an outbound HTTP request, or a JetStream publish or ack. The
//! property worth defending is not "some span exists somewhere"
//! — it is that NO effect surface is left dark, which is a statement about the
//! whole set of surfaces and cannot be observed by exercising one.
//!
//! Each surface keeps its OWN span name (`tracing` requires a constant one, and
//! `wamn.postgres` is published) while sharing one identity vocabulary from the
//! `effect_span!` macro. So this gate has two halves: every effect opens a span,
//! and the published names never move.
//!
//! Exercising the set at runtime is also not available: each of these surfaces
//! needs the live dependency behind it (a PostgreSQL, a NATS, an origin server)
//! before it reaches the instrumented await, so a subscriber-capture test would
//! prove the spans of whichever surfaces the environment happened to provide.
//! This reads the source instead and asserts the shape structurally, through
//! `syn` rather than text matching (the precedent is `effect_provider_revision`).
//!
//! The exception is [`the_published_postgres_identifiers_are_frozen`], which IS
//! a text assertion, because the thing it defends is text: the exact bytes a
//! Prometheus scrape and a Grafana panel match on.
//!
//! # The classification is exhaustive, which is what makes it hard to satisfy
//!
//! [`CONTRACT`] names every `impl … for ActiveCtx` in the four plugin files and
//! every method of each. A method the table does not classify fails the gate, and
//! so does a table entry with no method behind it — so adding a host function
//! forces a deliberate answer to "does this leave the host?", and deleting a span
//! from an existing one fails immediately.
//!
//! # Where the line between an effect and a local operation is drawn
//!
//! A method is an `Effect` when the guest invokes it and it leaves the host.
//! Three kinds of method do not:
//!
//! - **Resource destructors** (`drop`). The host reclaiming what the guest let
//!   go, not an operation the guest named; the WIT gives the guest no way to
//!   observe or handle the outcome, so there is no effect to attribute.
//! - **Accessors over already-delivered data** (`body`, `subject`, `headers`,
//!   `metadata`). The bytes are in host memory; reading them crosses nothing.
//! - **Host-local declarations** (`causation.set-run-context`). Writes a map
//!   inside the plugin.
//!
//! # Scope
//!
//! The four files `wamn-0h0g.24.3` instruments. This gate deliberately does not
//! sweep every plugin in the directory for `impl … for ActiveCtx`: that would
//! bind it to files this change does not own.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};
use syn::{Expr, ImplItem, Item, Macro, Type};

/// The only macro permitted to construct an effect span, and the only file
/// permitted to define it.
const SPAN_CONSTRUCTOR: &str = "effect_span";
const SPAN_MODULE: &str = "crates/platform/runtime/src/plugins/effect_span.rs";
const RUNTIME_SOURCE: &str = "crates/platform/runtime/src";

/// The span name each surface opens. Published identifiers: a Tempo panel in
/// `docs/archive/observability/dashboards.md` slices traces by them.
const SPAN_NAMES: &[(&str, &[&str])] = &[
    (POSTGRES, &["\"wamn.postgres\""]),
    (HTTP, &["\"wamn.http_effect\""]),
    (JETSTREAM, &["\"wamn.jetstream\""]),
];

const POSTGRES: &str = "crates/platform/runtime/src/plugins/wamn_postgres/resources.rs";
const HTTP: &str = "crates/platform/runtime/src/plugins/connection_http.rs";
const JETSTREAM: &str = "crates/platform/runtime/src/plugins/wamn_jetstream.rs";

/// How one host-trait method treats the world outside the host.
#[derive(Clone, Copy)]
enum Surface {
    /// A guest-invoked operation that leaves the host. Must open a span and
    /// instrument the awaited effect with it.
    Effect,
    /// Not an effect, carrying the reason so a failure explains the ruling.
    Local(&'static str),
}

const DESTRUCTOR: Surface =
    Surface::Local("resource destructor: the host reclaims what the guest let go");
const ACCESSOR: Surface = Surface::Local("accessor over already-delivered host memory");

/// The methods of one host trait, each classified as an effect or not.
type MethodSurfaces = &'static [(&'static str, Surface)];

/// Every `impl … for ActiveCtx` of the instrumented plugins, and every method.
const CONTRACT: &[(&str, &str, MethodSurfaces)] = &[
    (
        POSTGRES,
        "causation::Host",
        &[(
            "set_run_context",
            Surface::Local("host-local declaration into the plugin's per-component run map"),
        )],
    ),
    (
        POSTGRES,
        "client::Host",
        &[
            ("query", Surface::Effect),
            ("execute", Surface::Effect),
            ("begin", Surface::Effect),
        ],
    ),
    (
        POSTGRES,
        "client::HostTransaction",
        &[
            ("query", Surface::Effect),
            ("execute", Surface::Effect),
            ("open_cursor", Surface::Effect),
            ("commit", Surface::Effect),
            ("rollback", Surface::Effect),
            ("drop", DESTRUCTOR),
        ],
    ),
    (
        POSTGRES,
        "client::HostCursor",
        &[("fetch", Surface::Effect), ("drop", DESTRUCTOR)],
    ),
    (HTTP, "http_effect::Host", &[("send", Surface::Effect)]),
    (JETSTREAM, "consumer::Host", &[("bind", Surface::Effect)]),
    (
        JETSTREAM,
        "consumer::HostDurableConsumer",
        &[("fetch", Surface::Effect), ("drop", DESTRUCTOR)],
    ),
    (
        JETSTREAM,
        "consumer::HostMessage",
        &[
            ("body", ACCESSOR),
            ("subject", ACCESSOR),
            ("headers", ACCESSOR),
            ("metadata", ACCESSOR),
            ("ack", Surface::Effect),
            ("nack", Surface::Effect),
            ("term", Surface::Effect),
            ("drop", DESTRUCTOR),
        ],
    ),
    (JETSTREAM, "doorbell::Host", &[("ring", Surface::Effect)]),
    (JETSTREAM, "producer::Host", &[("publish", Surface::Effect)]),
];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repository_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn parse(relative: &str) -> syn::File {
    let source = read(relative);
    syn::parse_file(&source).unwrap_or_else(|error| panic!("parse {relative}: {error}"))
}

/// Every `.rs` file of the runtime crate, so "defined exactly once" means the
/// whole crate rather than the one file we expect it in.
fn runtime_sources() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut queue = vec![repository_root().join(RUNTIME_SOURCE)];
    while let Some(directory) = queue.pop() {
        let entries = fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("read dir {}: {error}", directory.display()));
        for entry in entries {
            let path = entry.expect("read directory entry").path();
            if path.is_dir() {
                queue.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files
}

/// What one syntax tree calls: functions, methods, and macros.
#[derive(Default)]
struct Calls {
    functions: BTreeSet<String>,
    methods: BTreeSet<String>,
    macros: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for Calls {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            Expr::Call(call) => {
                if let Expr::Path(path) = call.func.as_ref()
                    && let Some(last) = path.path.segments.last()
                {
                    self.functions.insert(last.ident.to_string());
                }
            }
            Expr::MethodCall(call) => {
                self.methods.insert(call.method.to_string());
            }
            _ => {}
        }
        visit::visit_expr(self, expr);
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        if let Some(last) = mac.path.segments.last() {
            self.macros.insert(last.ident.to_string());
        }
        visit::visit_macro(self, mac);
    }
}

/// Does this body reach the shared span constructor directly?
fn opens_a_span(calls: &Calls) -> bool {
    calls.macros.contains(SPAN_CONSTRUCTOR)
}

fn calls_of(block: &syn::Block) -> Calls {
    let mut calls = Calls::default();
    calls.visit_block(block);
    calls
}

/// Is `ty` the `ActiveCtx` the host traits are implemented for?
fn is_active_ctx(ty: &Type) -> bool {
    match ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "ActiveCtx"),
        _ => false,
    }
}

/// `client::HostTransaction` for `impl client::HostTransaction for ActiveCtx<'_>`.
fn trait_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// Every file-local function whose body reaches [`SPAN_CONSTRUCTOR`], so a
/// per-plugin wrapper (`db_span`, `js_span`, `invocation_span`) counts as
/// opening a span while an unrelated helper does not.
fn span_openers(file: &syn::File) -> BTreeSet<String> {
    let mut openers = BTreeSet::new();
    // One pass is enough for the wrappers in the tree; a wrapper of a wrapper
    // would need another, and would be a smell worth failing on anyway.
    for item in &file.items {
        if let Item::Fn(function) = item
            && opens_a_span(&calls_of(function.block.as_ref()))
        {
            openers.insert(function.sig.ident.to_string());
        }
    }
    openers
}

#[test]
fn every_effect_surface_opens_the_shared_span() {
    let mut expected: BTreeMap<&str, BTreeMap<&str, MethodSurfaces>> = BTreeMap::new();
    for &(file, host_trait, methods) in CONTRACT {
        assert!(
            expected
                .entry(file)
                .or_default()
                .insert(host_trait, methods)
                .is_none(),
            "{file}: {host_trait} is listed twice in CONTRACT"
        );
    }

    for (file, traits) in expected {
        let parsed = parse(file);
        let openers = span_openers(&parsed);

        let mut found: BTreeMap<String, Vec<&ImplItem>> = BTreeMap::new();
        for item in &parsed.items {
            let Item::Impl(implementation) = item else {
                continue;
            };
            let Some((_, path, _)) = &implementation.trait_ else {
                continue;
            };
            if !is_active_ctx(&implementation.self_ty) {
                continue;
            }
            found
                .entry(trait_path(path))
                .or_default()
                .extend(implementation.items.iter());
        }

        let found_traits: BTreeSet<&str> = found.keys().map(String::as_str).collect();
        let listed_traits: BTreeSet<&str> = traits.keys().copied().collect();
        assert_eq!(
            found_traits, listed_traits,
            "{file}: the `impl … for ActiveCtx` blocks in the source do not match CONTRACT. \
             Every host trait must be classified — a new one is a new effect surface until \
             someone says otherwise"
        );

        for (host_trait, methods) in traits {
            let items = &found[host_trait];
            let found_methods: BTreeSet<String> = items
                .iter()
                .filter_map(|item| match item {
                    ImplItem::Fn(function) => Some(function.sig.ident.to_string()),
                    _ => None,
                })
                .collect();
            let listed_methods: BTreeSet<String> = methods
                .iter()
                .map(|(name, _)| (*name).to_string())
                .collect();
            let classification: Vec<String> = methods
                .iter()
                .map(|(name, surface)| match surface {
                    Surface::Effect => format!("{name} — effect, must open a span"),
                    Surface::Local(reason) => format!("{name} — not an effect: {reason}"),
                })
                .collect();
            assert_eq!(
                found_methods, listed_methods,
                "{file}: the methods of `impl {host_trait} for ActiveCtx` do not match CONTRACT. \
                 Classify every host function as an effect or say why it is not one. \
                 Current ruling: {classification:#?}"
            );

            for &(name, surface) in methods {
                let Surface::Effect = surface else { continue };
                let function = items
                    .iter()
                    .find_map(|item| match item {
                        ImplItem::Fn(function) if function.sig.ident == name => Some(function),
                        _ => None,
                    })
                    .expect("method set was just proved equal");
                let calls = calls_of(&function.block);

                let via_wrapper = calls.functions.intersection(&openers).next().is_some();
                assert!(
                    opens_a_span(&calls) || via_wrapper,
                    "{file}: `{host_trait}::{name}` is an effect but opens no span. \
                     It must invoke `{SPAN_CONSTRUCTOR}!` or call a file-local wrapper of it \
                     (wrappers in this file: {openers:?})"
                );
                assert!(
                    calls.methods.contains("instrument"),
                    "{file}: `{host_trait}::{name}` builds a span but never calls \
                     `.instrument(..)`, so the span closes before the awaited effect runs \
                     and covers nothing"
                );
            }
        }
    }
}

#[test]
fn the_span_constructor_is_one_macro_that_owns_no_span_name() {
    let mut definitions: Vec<PathBuf> = Vec::new();
    let mut body = None;
    for path in runtime_sources() {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let parsed = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        for item in &parsed.items {
            let Item::Macro(item_macro) = item else {
                continue;
            };
            let is_macro_rules = item_macro
                .mac
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "macro_rules");
            if is_macro_rules
                && item_macro
                    .ident
                    .as_ref()
                    .is_some_and(|i| i == SPAN_CONSTRUCTOR)
            {
                definitions.push(path.clone());
                body = Some(item_macro.mac.tokens.to_string());
            }
        }
    }

    let located: Vec<String> = definitions
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert_eq!(
        definitions.len(),
        1,
        "`{SPAN_CONSTRUCTOR}!` must be defined exactly once so no surface can open a private \
         look-alike span; found {located:?}"
    );
    assert!(
        definitions[0].ends_with(SPAN_MODULE),
        "`{SPAN_CONSTRUCTOR}!` must live in {SPAN_MODULE}, found {}",
        definitions[0].display()
    );

    // The whole point of the macro: the NAME is the caller's, so no surface name
    // can be baked into the shared definition. `info_span !` is how a token
    // stream renders the invocation.
    let body = body.expect("definition was just counted");
    let after = body
        .split("info_span !")
        .nth(1)
        .expect("the macro opens a span with info_span!");
    // A token stream renders `$name` as `$ name`, so compare without spacing.
    let first = after.trim_start().trim_start_matches('(').trim_start();
    let squeezed: String = first.split_whitespace().collect();
    assert!(
        squeezed.starts_with("$name"),
        "`{SPAN_CONSTRUCTOR}!` must take its span name from its `$name` parameter, not bake one \
         in; it opens `info_span!({first:.40}…)`"
    );
}

#[test]
fn every_surface_uses_its_own_declared_span_name() {
    let mut found: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for &(file, expected) in SPAN_NAMES {
        let source = read(file);
        let parsed = syn::parse_file(&source).expect("plugin source parses");
        let mut names = SpanNames::default();
        names.visit_file(&parsed);
        found.insert(file, names.names.into_iter().collect());
        let _ = expected;
    }
    for &(file, expected) in SPAN_NAMES {
        let expected: BTreeSet<String> = expected.iter().map(|n| (*n).to_string()).collect();
        assert_eq!(
            found[file], expected,
            "{file}: the span names it opens changed. These are published identifiers — a Tempo \
             panel in docs/archive/observability/dashboards.md slices traces by span name."
        );
    }
}

/// The `wamn:postgres` span name, span fields, metric name and metric label are
/// published identifiers with live consumers OUTSIDE this crate. Renaming any of
/// them is an owner-level decision, never a refactor.
///
/// This gate exists because `wamn-0h0g.24.3` very nearly renamed all four while
/// unifying the span vocabulary. A `grep` for the dotted forms (`wamn.postgres`)
/// cannot find the consumers, because Prometheus scrapes them with underscores.
#[test]
fn the_published_postgres_identifiers_are_frozen() {
    let source = read(POSTGRES);
    for (fragment, consumer) in [
        (
            r#""wamn.postgres","#,
            "docs/archive/observability/dashboards.md:86 — Tempo TraceQL panel sliced by span name",
        ),
        (
            r#"db.system = "postgresql","#,
            "OTel DB semantic convention carried by the published span",
        ),
        (
            "db.operation = op,",
            "docs/archive/observability/dashboards.md:66 — Grafana panel sliced by db_operation",
        ),
        (
            r#".f64_histogram("wamn.postgres.query.duration_ms")"#,
            "tests/integration/src/metricbench.rs:721 (live poll, gate blocks on it) and :1251              (pinned assertion) read wamn_postgres_query_duration_ms_count",
        ),
        (
            r#"const DB_OPERATION: &str = "db.operation";"#,
            "tests/integration/src/metricbench.rs:1202 fixture carries db_operation=\"query\"",
        ),
    ] {
        assert!(
            source.contains(fragment),
            "{POSTGRES}: the published identifier {fragment:?} is gone. Consumer: {consumer}. \
             Renaming it breaks the in-cluster gate loudly and the deployed dashboards silently, \
             so it is the owner's call, not a refactor."
        );
    }
}

/// The span-name literal each `effect_span!` invocation opens with.
#[derive(Default)]
struct SpanNames {
    names: Vec<String>,
}

impl<'ast> Visit<'ast> for SpanNames {
    fn visit_macro(&mut self, mac: &'ast Macro) {
        let is_constructor = mac
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == SPAN_CONSTRUCTOR);
        if is_constructor {
            let first = mac
                .tokens
                .clone()
                .into_iter()
                .next()
                .expect("effect_span! takes a span name")
                .to_string();
            self.names.push(first);
        }
        visit::visit_macro(self, mac);
    }
}
