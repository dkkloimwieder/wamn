use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use wamn_schema_generator::client_ir::{ClientContractIr, ClientIrErrorKind, RouteIr, leaf_fields};

use super::fixture;

fn operation<'a>(
    ir: &'a ClientContractIr,
    model: &str,
    name: &str,
) -> &'a wamn_schema_generator::client_ir::OperationIr {
    ir.models
        .iter()
        .find(|candidate| candidate.name == model)
        .unwrap_or_else(|| panic!("missing model {model}"))
        .operations
        .iter()
        .find(|candidate| candidate.name == name)
        .unwrap_or_else(|| panic!("missing operation {model}/{name}"))
}

fn routes(ir: &ClientContractIr) -> BTreeMap<String, RouteIr> {
    ir.models
        .iter()
        .flat_map(|model| &model.operations)
        .filter_map(|operation| {
            operation
                .route
                .clone()
                .map(|route| (operation.operation.clone(), route))
        })
        .collect()
}

struct ReleaseFiles {
    root: PathBuf,
    contracts: PathBuf,
    attachments: PathBuf,
    published: BTreeMap<String, Value>,
}

impl ReleaseFiles {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "wamn-platform-client-ir-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let contracts = root.join("contracts");
        for (relative, bytes) in fixture::contracts(&fixture::generate_fixture()) {
            let path = contracts.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let published = routes(&fixture::client_release())
            .into_iter()
            .enumerate()
            .map(|(index, (operation, route))| {
                (
                    format!("widget-{index}"),
                    json!({
                        "kind": "http", "package-id": "platform_fixture",
                        "wiring-id": format!("widget-{index}"), "wiring-version": 1,
                        "definition-hash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "definition": {"id": format!("widget-{index}"), "kind": "http",
                            "route": {"method": route.method, "path": route.template}},
                        "auth-policy": {"modes": ["pat"]},
                        "registered-operation": operation
                    }),
                )
            })
            .collect();
        Self {
            attachments: root.join("attachments.json"),
            root,
            contracts,
            published,
        }
    }

    fn project(&self) -> Result<ClientContractIr, wamn_schema_generator::client_ir::ClientIrError> {
        std::fs::write(
            &self.attachments,
            serde_json::to_vec(&self.published).unwrap(),
        )
        .unwrap();
        ClientContractIr::from_release("platform_fixture", &self.contracts, &self.attachments)
    }
}

impl Drop for ReleaseFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn private_operations_are_excluded_but_malformed_public_operations_refuse() {
    let mut private = fixture::manifest();
    let archive = private["custom_operations"]["widget.archive"]
        .as_object_mut()
        .unwrap();
    archive.insert("visibility".to_owned(), json!("private"));
    archive.remove("permission");
    archive["errors"]
        .as_array_mut()
        .unwrap()
        .retain(|error| error != "permission_denied");
    let contracts = fixture::contracts(&fixture::generate_with(&fixture::catalog(), &private));
    let ir =
        ClientContractIr::from_release_contracts("platform_fixture", &contracts, &BTreeMap::new())
            .expect("private operation is excluded");
    assert!(
        ir.models
            .iter()
            .flat_map(|model| &model.operations)
            .all(|operation| operation.name != "archive")
    );
    assert_eq!(
        ir.models.iter().flat_map(|model| &model.operations).count(),
        5,
        "excluding one private operation must retain its public siblings"
    );

    let mut malformed = fixture::contracts(&fixture::generate_fixture());
    let operation = malformed.get_mut("widget/get.operation.json").unwrap();
    let mut document: Value = serde_json::from_slice(operation).unwrap();
    document.as_object_mut().unwrap().remove("grant");
    *operation = serde_json::to_vec(&document).unwrap();
    let refusal =
        ClientContractIr::from_release_contracts("platform_fixture", &malformed, &BTreeMap::new())
            .expect_err("a public operation without a grant refuses");
    assert_eq!(refusal.kind(), ClientIrErrorKind::MissingMember);
    assert!(refusal.to_string().contains("grant"), "{refusal}");
}

#[test]
fn platform_contract_shapes_project_exact_client_fields() {
    let contracts = fixture::contracts(&fixture::generate_fixture());
    let ir =
        ClientContractIr::from_release_contracts("platform_fixture", &contracts, &BTreeMap::new())
            .unwrap();
    let update = operation(&ir, "widget", "update");
    let leaves = leaf_fields(&update.input_fields);
    let paths: Vec<_> = leaves.iter().map(|field| field.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "change.code",
            "change.note",
            "expected_edit_version",
            "id",
            "request_id"
        ]
    );
    let by_path = |path: &str| leaves.iter().find(|field| field.path == path).unwrap();
    assert_eq!(by_path("id").type_name, "uuid");
    assert!(!by_path("id").nullable);
    assert!(by_path("expected_edit_version").revision);
    assert!(!by_path("change.code").required);
    assert!(!by_path("change.code").nullable);
    assert!(by_path("change.note").nullable);

    let archive = operation(&ir, "widget", "archive");
    assert_eq!(
        leaf_fields(&archive.input_fields)
            .iter()
            .map(|field| field.path.as_str())
            .collect::<Vec<_>>(),
        ["expected_edit_version", "id"]
    );
}

#[test]
fn every_platform_operation_has_constructible_input_and_typed_result() {
    let ir = fixture::client_release();
    for model in &ir.models {
        for operation in &model.operations {
            assert!(
                !operation.input_fields.is_empty(),
                "{}/{}",
                model.name,
                operation.name
            );
            assert_ne!(
                operation.result_class, "none",
                "{}/{}",
                model.name, operation.name
            );
            assert!(
                !operation.result_fields.is_empty(),
                "{}/{}",
                model.name,
                operation.name
            );
            assert!(!operation.permission_token.is_empty(), "{operation:?}");
            assert!(!operation.grant.is_empty(), "{operation:?}");
            assert!(operation.operation.contains(':'), "{operation:?}");
        }
    }
}

#[test]
fn platform_release_routes_are_exact_and_contracts_alone_invent_none() {
    let released = fixture::client_release();
    let get = operation(&released, "widget", "get");
    assert_eq!(get.route.as_ref().unwrap().method, "POST");
    assert_eq!(get.route.as_ref().unwrap().template, "/widget/get");
    assert_eq!(
        released
            .models
            .iter()
            .flat_map(|model| &model.operations)
            .filter(|operation| operation.route.is_some())
            .count(),
        6
    );

    let contracts = fixture::contracts(&fixture::generate_fixture());
    let contracts_only =
        ClientContractIr::from_release_contracts("platform_fixture", &contracts, &BTreeMap::new())
            .expect("platform contracts project without publication routes");
    assert!(
        contracts_only
            .models
            .iter()
            .flat_map(|model| &model.operations)
            .all(|op| op.route.is_none())
    );
}

#[test]
fn attachment_routes_refuse_ambiguity_and_unnormalized_methods() {
    let mut files = ReleaseFiles::new();
    let (id, mut alias) = files
        .published
        .iter()
        .next()
        .map(|(id, value)| (id.clone(), value.clone()))
        .unwrap();
    alias["definition"]["route"]["path"] = json!("/v2/widget/get");
    files.published.insert(format!("{id}-alias"), alias);
    let refusal = files
        .project()
        .expect_err("one operation at two paths refuses");
    assert_eq!(refusal.kind(), ClientIrErrorKind::AmbiguousRoute);
    assert!(refusal.to_string().contains("/v2/widget/get"), "{refusal}");

    files.published.remove(&format!("{id}-alias"));
    files.published.get_mut(&id).unwrap()["definition"]["route"]["method"] = json!("post");
    let refusal = files.project().expect_err("a lowercase method refuses");
    assert_eq!(refusal.kind(), ClientIrErrorKind::UnnormalizedRoute);
    assert!(refusal.to_string().contains("POST"), "{refusal}");
}

#[test]
fn studio_attachment_removes_exactly_one_client_route() {
    let mut files = ReleaseFiles::new();
    let baseline = files.project().unwrap();
    let id = files.published.keys().next().unwrap().clone();
    files.published.get_mut(&id).unwrap()["kind"] = json!("studio");
    let changed = files.project().unwrap();
    let count = |ir: &ClientContractIr| {
        ir.models
            .iter()
            .flat_map(|model| &model.operations)
            .filter(|operation| operation.route.is_some())
            .count()
    };
    assert_eq!(count(&changed) + 1, count(&baseline));
}

#[test]
fn parameterized_routes_survive_verbatim() {
    let contracts = fixture::contracts(&fixture::generate_fixture());
    let baseline = fixture::client_release();
    let mut changed_routes = routes(&baseline);
    let get_identity = operation(&baseline, "widget", "get").operation.clone();
    changed_routes.get_mut(&get_identity).unwrap().template =
        "/tenant/{tenant}/widget/{id}".to_owned();
    let changed =
        ClientContractIr::from_release_contracts("platform_fixture", &contracts, &changed_routes)
            .expect("parameterized route projects");
    assert_eq!(
        operation(&changed, "widget", "get")
            .route
            .as_ref()
            .unwrap()
            .template,
        "/tenant/{tenant}/widget/{id}"
    );
    assert_ne!(
        operation(&baseline, "widget", "get").route,
        operation(&changed, "widget", "get").route
    );
}

#[test]
fn client_ir_is_canonical_and_surfaces_contract_changes() {
    let package = fixture::generate_fixture();
    let contracts = fixture::contracts(&package);
    let route_map = routes(&fixture::client_release());
    let straight =
        ClientContractIr::from_release_contracts("platform_fixture", &contracts, &route_map)
            .unwrap();
    assert_eq!(
        straight.canonical_bytes(),
        fixture::client_release().canonical_bytes()
    );
    assert!(!straight.canonical_bytes().is_empty());

    let mut reordered = contracts.clone();
    for bytes in reordered.values_mut() {
        let mut document: Value = serde_json::from_slice(bytes).unwrap();
        reverse_unordered_lists(&mut document);
        *bytes = serde_json::to_vec(&document).unwrap();
    }
    let reordered =
        ClientContractIr::from_release_contracts("platform_fixture", &reordered, &route_map)
            .unwrap();
    assert_eq!(
        straight.canonical_bytes(),
        reordered.canonical_bytes(),
        "unordered contract members must not change canonical IR bytes"
    );

    let mut changed = contracts.clone();
    let result = changed.get_mut("widget/archive.result.json").unwrap();
    let mut document: Value = serde_json::from_slice(result).unwrap();
    document["fields"].as_array_mut().unwrap().push(json!({
        "path": "review_note", "type": "text", "nullable": true, "values": []
    }));
    *result = serde_json::to_vec(&document).unwrap();
    let changed =
        ClientContractIr::from_release_contracts("platform_fixture", &changed, &route_map).unwrap();
    assert_ne!(straight.canonical_bytes(), changed.canonical_bytes());
    assert!(
        changed
            .models
            .iter()
            .flat_map(|model| &model.fields)
            .any(|field| field.path == "review_note")
    );
}

fn reverse_unordered_lists(value: &mut Value) {
    if let Value::Object(members) = value {
        for (name, member) in members {
            if [
                "fields",
                "columns",
                "binds",
                "cases",
                "filters",
                "statements",
            ]
            .contains(&name.as_str())
                && let Value::Array(items) = member
            {
                items.reverse();
            }
            reverse_unordered_lists(member);
        }
    }
}

#[test]
fn paging_and_closed_values_reach_the_platform_ir() {
    let mut contracts = fixture::contracts(&fixture::generate_fixture());
    let result = contracts.get_mut("widget/get.result.json").unwrap();
    let mut document: Value = serde_json::from_slice(result).unwrap();
    document["fields"].as_array_mut().unwrap()[0]["values"] = json!(["alpha", "beta"]);
    *result = serde_json::to_vec(&document).unwrap();
    let ir = ClientContractIr::from_release_contracts(
        "platform_fixture",
        &contracts,
        &routes(&fixture::client_release()),
    )
    .unwrap();
    assert_eq!(
        operation(&ir, "widget", "get").result_fields[0].values,
        ["alpha", "beta"]
    );

    let query = operation(&ir, "widget", "query");
    let paging = query.paging.as_ref().expect("query pages");
    assert!(!paging.filters.is_empty());
    assert!(paging.pagination.is_some());
    assert_eq!(query.result_class, "page");
    assert!(operation(&ir, "widget", "get").paging.is_none());
}
