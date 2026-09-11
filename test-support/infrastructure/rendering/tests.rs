use super::*;

const BASE: &str = include_str!("../../../deploy/platform/values-host-default.yaml");
const RECEIVING: &str = include_str!("../../../deploy/platform/values-host-receiving-pat.yaml");
const WMS: &str = include_str!("../../../deploy/platform/values-host-wms-pat.yaml");
const HTTP: &str = include_str!("../../../deploy/platform/http-route-workload.example.yaml");
const MATERIALIZER: &str = include_str!("../../../deploy/platform/materializer.example.yaml");
const KIND: &str = include_str!("../../../deploy/infra/kind-config.yaml");

fn event(project: &str) -> EventIdentity {
    EventIdentity {
        org: "acme".into(),
        project: project.into(),
        environment: "dev".into(),
    }
}

fn host_input(project: &str) -> HostValuesInput {
    HostValuesInput {
        namespace: "warehouse-eu-3".into(),
        host_tag: "sha-9f3c1ab".into(),
        replicas: 3,
        component_artifact_base: "registry.test.invalid:5000/components".into(),
        release_artifact_base: "registry.test.invalid:5000/releases".into(),
        manifest_digest: "sha256:0123456789abcdef".into(),
        nats_url: "nats://nats.test.invalid:4222".into(),
        stream_replicas: 1,
        dup_window_secs: 120,
        event: event(project),
        guest_secret_name: "test-guest-sql".into(),
        role_secrets: [
            WorkloadRoleFamily::ExecutorPlatform,
            WorkloadRoleFamily::IdentityReader,
            WorkloadRoleFamily::HttpAdmitter,
            WorkloadRoleFamily::EventMaterializer,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, family)| HostRoleSecret {
            family,
            name: format!("test-role-{index}"),
        })
        .collect(),
        object_store_secret_name: (project == "wms").then(|| "test-object-store".into()),
    }
}

fn value<'a>(env: &'a [EnvVar], name: &str) -> &'a str {
    env.iter()
        .find(|entry| entry.name == name)
        .unwrap()
        .value
        .as_deref()
        .unwrap()
}

#[test]
fn host_values_keep_both_applications_separate_and_preserve_credentials() {
    for (project, template) in [("receiving", RECEIVING), ("wms", WMS)] {
        let input = host_input(project);
        let output = render_host_values(BASE, template, &input).unwrap();
        let base: HostValues = serde_yaml::from_str(&output.base).unwrap();
        let overlay: HostValues = serde_yaml::from_str(&output.overlay).unwrap();
        let original: HostValues = serde_yaml::from_str(template).unwrap();
        assert_eq!(base.runtime.image.as_ref().unwrap().tag, input.host_tag);
        assert_eq!(base.runtime.host_groups[0].namespace, input.namespace);
        assert_eq!(base.runtime.host_groups[0].replicas, 3);
        let group = &overlay.runtime.host_groups[0];
        let old = &original.runtime.host_groups[0];
        assert_eq!(group.namespace, input.namespace);
        assert_eq!(group.replicas, 3);
        assert_eq!(group.extra, old.extra);
        for (name, expected) in [
            (
                "WAMN_COMPONENT_ARTIFACT_BASE",
                input.component_artifact_base.as_str(),
            ),
            ("WAMN_EVT_NATS_URL", input.nats_url.as_str()),
            ("WAMN_EVT_STREAM_REPLICAS", "1"),
            ("WAMN_EVT_DUP_WINDOW_SECS", "120"),
            ("WAMN_EVT_ORG", "acme"),
            ("WAMN_EVT_PROJECT", project),
            ("WAMN_EVT_ENV", "dev"),
            ("WAMN_PROJECT", project),
            ("OTEL_BSP_SCHEDULE_DELAY", "1"),
            ("OTEL_BSP_MAX_EXPORT_BATCH_SIZE", "1"),
            (
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                "http://otel-collector.wamn-system.svc.cluster.local:4317",
            ),
            (
                "WAMN_EVT_NATS_PASSWORD_FILE",
                "/etc/wamn/event-nats/password",
            ),
            (
                "WAMN_MAT_NATS_BINDING_FILE",
                "/etc/wamn/materializer-nats/binding.json",
            ),
        ] {
            assert_eq!(value(&group.env, name), expected, "{project}: {name}");
        }
        let guest = group
            .env
            .iter()
            .find(|entry| entry.name == "WAMN_PG_URL")
            .unwrap();
        assert_eq!(
            guest.value_from.as_ref().unwrap().secret_key_ref.name,
            input.guest_secret_name
        );
        for selected in &input.role_secrets {
            assert_eq!(
                group
                    .env
                    .iter()
                    .filter_map(|entry| entry.value_from.as_ref())
                    .filter(|source| source.secret_key_ref.name == selected.name)
                    .count(),
                1
            );
        }
        for name in ["event-nats", "materializer-nats", "registry-pull"] {
            let rendered = group
                .volumes
                .iter()
                .find(|volume| volume.name == name)
                .unwrap();
            let before = old
                .volumes
                .iter()
                .find(|volume| volume.name == name)
                .unwrap();
            assert_eq!(rendered.secret, before.secret, "{name}");
        }
        let event_user = group
            .env
            .iter()
            .find(|entry| entry.name == "WAMN_EVT_NATS_USERNAME")
            .unwrap();
        assert_eq!(
            event_user.value_from.as_ref().unwrap().secret_key_ref.name,
            "wamn-event-nats"
        );
        assert_eq!(
            event_user.value_from.as_ref().unwrap().secret_key_ref.key,
            "username"
        );
        assert_eq!(
            event_user
                .value_from
                .as_ref()
                .unwrap()
                .secret_key_ref
                .optional,
            Some(false)
        );
        if let Some(name) = &input.object_store_secret_name {
            let volume = group
                .volumes
                .iter()
                .find(|volume| volume.name == "connection-credentials")
                .unwrap();
            assert_eq!(&volume.secret.as_ref().unwrap().secret_name, name);
        }
        assert_eq!(
            group.extra_args,
            [
                format!("--release-artifact-base={}", input.release_artifact_base),
                format!("--release-manifest-digest={}", input.manifest_digest),
                "--allow-insecure-registries".into()
            ]
        );
    }
}

#[test]
fn startup_host_values_change_only_replica_counts() {
    let normal_input = host_input("receiving");
    let normal = render_host_values(BASE, RECEIVING, &normal_input).unwrap();
    let mut startup_input = normal_input.clone();
    startup_input.replicas = 0;
    let startup = render_host_values(BASE, RECEIVING, &startup_input).unwrap();
    for (normal, startup) in [
        (normal.base, startup.base),
        (normal.overlay, startup.overlay),
    ] {
        let normal: Value = serde_yaml::from_str(&normal).unwrap();
        let mut startup: Value = serde_yaml::from_str(&startup).unwrap();
        assert_eq!(
            startup["runtime"]["hostGroups"][0]["replicas"],
            Value::from(0)
        );
        startup["runtime"]["hostGroups"][0]["replicas"] = Value::from(3);
        assert_eq!(startup, normal);
    }
}

#[test]
fn host_values_refuse_missing_and_duplicate_owned_fields() {
    let input = host_input("receiving");
    let original: HostValues = serde_yaml::from_str(RECEIVING).unwrap();
    for name in [
        "WAMN_PG_URL",
        "WAMN_COMPONENT_ARTIFACT_BASE",
        "WAMN_EVT_NATS_URL",
        "WAMN_EVT_ORG",
        "WAMN_EVT_PROJECT",
        "WAMN_EVT_ENV",
        "WAMN_WASMTIME_CACHE_DIR",
    ] {
        let mut document = original.clone();
        document.runtime.host_groups[0]
            .env
            .retain(|entry| entry.name != name);
        let error = render_host_values(BASE, &serde_yaml::to_string(&document).unwrap(), &input)
            .unwrap_err();
        assert!(error.to_string().contains(name), "{error:#}");
    }
    let mut duplicate = original.clone();
    let variable = duplicate.runtime.host_groups[0].env[0].clone();
    duplicate.runtime.host_groups[0].env.push(variable);
    assert!(
        render_host_values(BASE, &serde_yaml::to_string(&duplicate).unwrap(), &input)
            .unwrap_err()
            .to_string()
            .contains("repeats environment variable")
    );
    let mut missing_role = original.clone();
    missing_role.runtime.host_groups[0]
        .env
        .retain(|entry| entry.name != "WAMN_HTTP_ADMITTER_PG_URL");
    assert!(
        render_host_values(BASE, &serde_yaml::to_string(&missing_role).unwrap(), &input)
            .unwrap_err()
            .to_string()
            .contains("missing role Secret")
    );
    let mut repeated_group = original.clone();
    repeated_group
        .runtime
        .host_groups
        .push(original.runtime.host_groups[0].clone());
    assert!(
        render_host_values(
            BASE,
            &serde_yaml::to_string(&repeated_group).unwrap(),
            &input
        )
        .unwrap_err()
        .to_string()
        .contains("exactly one")
    );
    for argument in ["--release-artifact-base", "--release-manifest-digest"] {
        let mut document = original.clone();
        document.runtime.host_groups[0]
            .extra_args
            .retain(|arg| !arg.starts_with(argument));
        assert!(
            render_host_values(BASE, &serde_yaml::to_string(&document).unwrap(), &input)
                .unwrap_err()
                .to_string()
                .contains(argument)
        );
    }
}

#[test]
fn host_object_credentials_must_match_the_selected_application() {
    let mut input = host_input("wms");
    input.object_store_secret_name = None;
    assert!(
        render_host_values(BASE, WMS, &input)
            .unwrap_err()
            .to_string()
            .contains("none were declared")
    );
    let mut input = host_input("receiving");
    input.object_store_secret_name = Some("test-object-store".into());
    assert!(
        render_host_values(BASE, RECEIVING, &input)
            .unwrap_err()
            .to_string()
            .contains("no declared object-store")
    );
}

fn http_input(project: &str, catalog: &str) -> HttpWorkloadInput {
    HttpWorkloadInput {
        namespace: "warehouse-eu-3".into(),
        image: "registry.test.invalid:5000/flow-http@sha256:abc123".into(),
        route_host: "route.test.invalid".into(),
        claims: HttpClaims {
            tenant: format!("{project}-route-auth"),
            catalog: catalog.into(),
            environment: "dev".into(),
            project: project.into(),
            schema: project.into(),
        },
    }
}

fn http_documents(yaml: &str) -> Vec<HttpDocument> {
    serde_yaml::Deserializer::from_str(yaml)
        .map(|document| HttpDocument::deserialize(document).unwrap())
        .collect()
}

#[test]
fn http_workload_renders_both_namespaces_all_claims_and_one_route() {
    for (project, catalog) in [("receiving", "default"), ("wms", "wms-catalog")] {
        let input = http_input(project, catalog);
        let rendered = render_http_workload(HTTP, &input).unwrap();
        let original = http_documents(HTTP);
        let documents = http_documents(&rendered);
        let HttpDocument::Service(service) = &documents[0] else {
            panic!("Service")
        };
        let HttpDocument::Service(original_service) = &original[0] else {
            panic!("Service")
        };
        assert_eq!(service.metadata.namespace, input.namespace);
        assert_eq!(service.extra, original_service.extra);
        let HttpDocument::WorkloadDeployment(workload) = &documents[1] else {
            panic!("workload")
        };
        assert_eq!(workload.metadata.namespace, input.namespace);
        assert_eq!(workload.spec.template.spec.environment, input.namespace);
        let component = &workload.spec.template.spec.components[0];
        assert_eq!(component.image, input.image);
        assert_eq!(component.local_resources.config, input.claims);
        let interfaces = &workload.spec.template.spec.host_interfaces;
        assert_eq!(
            interfaces
                .iter()
                .filter(|interface| interface.config.is_some())
                .count(),
            1
        );
        let http = interfaces
            .iter()
            .find(|interface| interface.namespace == "wasi" && interface.package == "http")
            .unwrap();
        assert_eq!(http.version, "0.3.0");
        assert_eq!(http.interfaces, ["handler"]);
        assert_eq!(http.config.as_ref().unwrap().host, input.route_host);
    }
}

#[test]
fn http_workload_refuses_incomplete_claims_unknown_claims_and_duplicate_fields() {
    let input = http_input("receiving", "default");
    for field in [
        "wamn.tenant",
        "wamn.catalog",
        "wamn.environment",
        "wamn.project",
        "wamn.schema",
    ] {
        let mut values = serde_yaml::Deserializer::from_str(HTTP)
            .map(|doc| Value::deserialize(doc).unwrap())
            .collect::<Vec<_>>();
        values[1]["spec"]["template"]["spec"]["components"][0]["localResources"]["config"]
            .as_mapping_mut()
            .unwrap()
            .remove(Value::from(field));
        let changed = values
            .iter()
            .map(|value| serde_yaml::to_string(value).unwrap())
            .collect::<Vec<_>>()
            .join("---\n");
        assert!(
            format!("{:#}", render_http_workload(&changed, &input).unwrap_err()).contains(field)
        );
    }
    let bad = HTTP.replace(
        "wamn.catalog: \"default\"",
        "wamn.catalog: \"default\"\n              wamn.legacy_catalog: \"default\"",
    );
    assert!(
        format!("{:#}", render_http_workload(&bad, &input).unwrap_err())
            .contains("wamn.legacy_catalog")
    );
    let duplicate = HTTP.replace(
        "wamn.catalog: \"default\"",
        "wamn.catalog: \"default\"\n              wamn.catalog: \"default\"",
    );
    assert!(render_http_workload(&duplicate, &input).is_err());
}

#[test]
fn http_workload_requires_the_service_workload_and_http_handler_once() {
    let input = http_input("receiving", "default");
    let original = http_documents(HTTP);
    let only_service = serde_yaml::to_string(&original[0]).unwrap();
    assert!(render_http_workload(&only_service, &input).is_err());
    let twice = format!("{only_service}---\n{only_service}");
    assert!(render_http_workload(&twice, &input).is_err());
    for duplicate in [false, true] {
        let mut documents = original.clone();
        let HttpDocument::WorkloadDeployment(workload) = &mut documents[1] else {
            panic!("workload")
        };
        let interfaces = &mut workload.spec.template.spec.host_interfaces;
        if duplicate {
            interfaces.push(interfaces[0].clone());
        } else {
            interfaces.remove(0);
        }
        let changed = documents
            .iter()
            .map(|doc| serde_yaml::to_string(doc).unwrap())
            .collect::<Vec<_>>()
            .join("---\n");
        assert!(
            render_http_workload(&changed, &input)
                .unwrap_err()
                .to_string()
                .contains("wasi:http")
        );
    }
}

fn materializer_input() -> MaterializerInput {
    MaterializerInput {
        workload: "receiving-materializer".into(),
        namespace: "warehouse-eu-3".into(),
        image: "registry.test.invalid:5000/materializer@sha256:abc123".into(),
        tenant: "receiving-route-auth".into(),
        event: event("receiving"),
        event_stream: "EVT_4_acme_9_receiving_3_dev".into(),
        fetch_ms: 500,
        sweep_ms: 500,
    }
}

#[test]
fn materializer_renders_each_identity_and_preserves_native_binding_and_retry() {
    let receiving = materializer_input();
    let second = MaterializerInput {
        workload: "hopper-materializer".into(),
        tenant: "quay-9-route-auth".into(),
        event: EventIdentity {
            org: "zamboni".into(),
            project: "quay9".into(),
            environment: "stage7".into(),
        },
        event_stream: "EVT_7_zamboni_5_quay9_6_stage7".into(),
        fetch_ms: 131,
        sweep_ms: 137,
        ..receiving.clone()
    };
    let original: MaterializerDocument = serde_yaml::from_str(MATERIALIZER).unwrap();
    for input in [receiving, second] {
        let rendered = render_materializer(MATERIALIZER, &input).unwrap();
        let document: MaterializerDocument = serde_yaml::from_str(&rendered).unwrap();
        assert_eq!(document.metadata.name, input.workload);
        assert_eq!(document.metadata.namespace, input.namespace);
        let spec = &document.spec.template.spec;
        assert_eq!(spec.environment, input.namespace);
        assert_eq!(spec.service.image, input.image);
        let claims = &spec.service.local_resources.config;
        assert_eq!(claims.tenant, input.tenant);
        assert_eq!(claims.project, input.event.project);
        assert_eq!(claims.environment, input.event.environment);
        assert_eq!(claims.authority, MaterializerAuthority::EventMaterializer);
        let environment = &spec.service.local_resources.environment.config;
        assert_eq!(environment.stream, input.event_stream);
        assert_eq!(environment.org, input.event.org);
        assert_eq!(environment.project, input.event.project);
        assert_eq!(environment.environment, input.event.environment);
        assert_eq!(environment.tenant, input.tenant);
        assert_eq!(
            environment.fetch_ms.as_deref(),
            Some(input.fetch_ms.to_string().as_str())
        );
        assert_eq!(
            environment.sweep_ms.as_deref(),
            Some(input.sweep_ms.to_string().as_str())
        );
        assert_eq!(environment.max_deliver, "5");
        assert_eq!(
            spec.host_interfaces,
            original.spec.template.spec.host_interfaces
        );
        let native = spec
            .host_interfaces
            .iter()
            .find(|interface| interface.namespace == "wasmcloud" && interface.package == "nats")
            .unwrap();
        assert_eq!(native.name.as_deref(), Some("events"));
        assert_eq!(native.interfaces, ["types", "jetstream"]);
        let registration = spec
            .host_interfaces
            .iter()
            .find(|interface| interface.namespace == "wamn" && interface.package == "jetstream")
            .unwrap();
        assert_eq!(registration.interfaces, ["types", "registration"]);
    }
}

#[test]
fn materializer_refuses_missing_identity_extra_subscription_fields_and_credentials() {
    let input = materializer_input();
    for field in [
        "WAMN_MAT_STREAM",
        "WAMN_MAT_ORG",
        "WAMN_MAT_PROJECT",
        "WAMN_MAT_ENV",
        "WAMN_MAT_TENANT",
    ] {
        let mut document: Value = serde_yaml::from_str(MATERIALIZER).unwrap();
        document["spec"]["template"]["spec"]["service"]["localResources"]["environment"]["config"]
            .as_mapping_mut()
            .unwrap()
            .remove(Value::from(field));
        assert!(
            format!(
                "{:#}",
                render_materializer(&serde_yaml::to_string(&document).unwrap(), &input)
                    .unwrap_err()
            )
            .contains(field)
        );
    }
    for field in [
        "WAMN_MAT_LEGACY_TENANT",
        "WAMN_EVT_NATS_PASSWORD",
        "WAMN_MAT_NATS_BINDING_FILE",
    ] {
        let mut document: Value = serde_yaml::from_str(MATERIALIZER).unwrap();
        document["spec"]["template"]["spec"]["service"]["localResources"]["environment"]["config"]
            [field] = Value::from("t1");
        assert!(
            format!(
                "{:#}",
                render_materializer(&serde_yaml::to_string(&document).unwrap(), &input)
                    .unwrap_err()
            )
            .contains(field)
        );
    }
}

#[test]
fn yaml_serialization_keeps_caller_values_as_single_values() {
    let mut input = http_input("receiving", "default");
    input.route_host = "host: quoted\nsecond-line".into();
    input.claims.catalog = "catalog: \"quoted\" # value".into();
    let documents = http_documents(&render_http_workload(HTTP, &input).unwrap());
    let HttpDocument::WorkloadDeployment(workload) = &documents[1] else {
        panic!("workload")
    };
    assert_eq!(
        workload.spec.template.spec.host_interfaces[0]
            .config
            .as_ref()
            .unwrap()
            .host,
        input.route_host
    );
    assert_eq!(
        workload.spec.template.spec.components[0]
            .local_resources
            .config
            .catalog,
        input.claims.catalog
    );
}

#[test]
fn kind_configuration_keeps_three_nodes_without_host_port_mappings() {
    let original: KindCluster = serde_yaml::from_str(KIND).unwrap();
    let rendered: KindCluster = serde_yaml::from_str(&render_kind_cluster(KIND).unwrap()).unwrap();
    assert_eq!(rendered.nodes.len(), 3);
    assert_eq!(rendered.extra, original.extra);
    assert_eq!(
        rendered
            .nodes
            .iter()
            .map(|node| &node.role)
            .collect::<Vec<_>>(),
        [
            &NodeRole::ControlPlane,
            &NodeRole::Worker,
            &NodeRole::Worker
        ]
    );
    assert!(
        rendered
            .nodes
            .iter()
            .all(|node| node.extra_port_mappings.is_empty())
    );
    let mut too_few = original;
    too_few.nodes.pop();
    assert!(
        render_kind_cluster(&serde_yaml::to_string(&too_few).unwrap())
            .unwrap_err()
            .to_string()
            .contains("three nodes")
    );
}

#[test]
fn workload_documents_refuse_missing_required_fields_and_wrong_types() {
    let input = materializer_input();
    for path in [
        vec!["metadata", "name"],
        vec!["metadata", "namespace"],
        vec!["spec", "template", "spec", "environment"],
        vec!["spec", "template", "spec", "service", "image"],
        vec![
            "spec",
            "template",
            "spec",
            "service",
            "localResources",
            "config",
            "wamn.tenant",
        ],
        vec![
            "spec",
            "template",
            "spec",
            "service",
            "localResources",
            "config",
            "wamn.project",
        ],
        vec![
            "spec",
            "template",
            "spec",
            "service",
            "localResources",
            "config",
            "wamn.environment",
        ],
    ] {
        let mut document: Value = serde_yaml::from_str(MATERIALIZER).unwrap();
        let mut parent = &mut document;
        for part in &path[..path.len() - 1] {
            parent = &mut parent[*part];
        }
        parent
            .as_mapping_mut()
            .unwrap()
            .remove(Value::from(*path.last().unwrap()));
        assert!(
            render_materializer(&serde_yaml::to_string(&document).unwrap(), &input).is_err(),
            "{path:?}"
        );
    }
    let mut document: Value = serde_yaml::from_str(BASE).unwrap();
    document["runtime"]["hostGroups"][0]["replicas"] = Value::from("three");
    assert!(
        render_host_values(
            &serde_yaml::to_string(&document).unwrap(),
            RECEIVING,
            &host_input("receiving")
        )
        .is_err()
    );
}

#[test]
fn http_template_cannot_carry_a_second_route_configuration() {
    let mut documents = http_documents(HTTP);
    let HttpDocument::WorkloadDeployment(workload) = &mut documents[1] else {
        panic!("workload")
    };
    workload.spec.template.spec.host_interfaces[1].config = Some(HttpRoute {
        host: "route.test.invalid".into(),
    });
    let changed = documents
        .iter()
        .map(|doc| serde_yaml::to_string(doc).unwrap())
        .collect::<Vec<_>>()
        .join("---\n");
    assert!(
        render_http_workload(&changed, &http_input("receiving", "default"))
            .unwrap_err()
            .to_string()
            .contains("already declares route")
    );
}

#[test]
fn host_identity_accepts_matching_apps_and_refuses_crossed_declarations() {
    let receiving = HostIdentity {
        org: "acme".into(),
        project: "receiving".into(),
        schema: "receiving".into(),
    };
    let wms = HostIdentity {
        org: "acme".into(),
        project: "wms".into(),
        schema: "wms".into(),
    };
    assert_rendered_identity(RECEIVING, &receiving).unwrap();
    assert_rendered_identity(WMS, &wms).unwrap();
    assert!(
        assert_rendered_identity(RECEIVING, &wms)
            .unwrap_err()
            .to_string()
            .contains("claims WAMN_PROJECT=receiving")
    );
    assert!(
        assert_rendered_identity(WMS, &receiving)
            .unwrap_err()
            .to_string()
            .contains("claims WAMN_PROJECT=wms")
    );
}

#[test]
fn host_identity_reports_each_missing_repeated_or_wrong_claim() {
    let identity = HostIdentity {
        org: "acme".into(),
        project: "receiving".into(),
        schema: "receiving".into(),
    };
    let original: HostValues = serde_yaml::from_str(RECEIVING).unwrap();
    for name in ["WAMN_ORG", "WAMN_PROJECT", "WAMN_SCHEMA"] {
        let mut missing = original.clone();
        missing.runtime.host_groups[0]
            .env
            .retain(|entry| entry.name != name);
        assert!(
            assert_rendered_identity(&serde_yaml::to_string(&missing).unwrap(), &identity)
                .unwrap_err()
                .to_string()
                .contains(&format!("declares no {name}"))
        );
        for value in ["acme", "globex"] {
            let mut repeated = original.clone();
            let mut variable = repeated.runtime.host_groups[0]
                .env
                .iter()
                .find(|entry| entry.name == name)
                .unwrap()
                .clone();
            variable.value = Some(value.into());
            repeated.runtime.host_groups[0].env.push(variable);
            assert!(
                assert_rendered_identity(&serde_yaml::to_string(&repeated).unwrap(), &identity)
                    .unwrap_err()
                    .to_string()
                    .contains(&format!("declares {name} more than once"))
            );
        }
        let mut wrong = original.clone();
        env_mut(&mut wrong.runtime.host_groups[0].env, name)
            .unwrap()
            .value = Some("wrong".into());
        assert!(
            assert_rendered_identity(&serde_yaml::to_string(&wrong).unwrap(), &identity)
                .unwrap_err()
                .to_string()
                .contains(&format!("claims {name}=wrong"))
        );
    }
}

#[test]
fn host_identity_matches_whole_names_and_accepts_equivalent_yaml_layouts() {
    let identity = HostIdentity {
        org: "acme".into(),
        project: "receiving".into(),
        schema: "receiving".into(),
    };
    let mut document: HostValues = serde_yaml::from_str(RECEIVING).unwrap();
    for (name, value) in [("WAMN_ORG_LEGACY", "globex"), ("WAMN_NOTE", "WAMN_ORG")] {
        document.runtime.host_groups[0].env.push(EnvVar {
            name: name.into(),
            value: Some(value.into()),
            value_from: None,
            extra: BTreeMap::new(),
        });
    }
    let block_yaml = serde_yaml::to_string(&document).unwrap();
    assert_rendered_identity(&block_yaml, &identity).unwrap();
    assert_rendered_identity(RECEIVING, &identity).unwrap();
    assert!(assert_rendered_identity("not YAML: [", &identity).is_err());
}
