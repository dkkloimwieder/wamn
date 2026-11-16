//! Index the catalog graph without copying its nodes or edges.
use anyhow::Context as _;
use boon::{Compiler, Draft, SchemaIndex, Schemas};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use wamn_catalog::{AdmittedComponent, WiringDocument, WiringEdge, WiringNode, resolve_input_port};

pub(super) const DEFAULT_HOP_LIMIT: u64 = 10_000;

pub(crate) struct Wiring {
    document: WiringDocument,
    out: HashMap<String, Vec<usize>>,
    inferred_ports: HashMap<usize, String>,
    input_schemas: HashMap<String, HashMap<String, PortSchema>>,
    output_schemas: HashMap<String, PortSchema>,
}

struct PortSchema {
    schemas: Schemas,
    index: SchemaIndex,
}

impl Wiring {
    pub(crate) fn compile(
        document: WiringDocument,
        components: &BTreeMap<String, AdmittedComponent>,
    ) -> anyhow::Result<Self> {
        let mut out: HashMap<String, Vec<usize>> = HashMap::new();
        let mut inferred_ports = HashMap::new();
        for (index, edge) in document.edges.iter().enumerate() {
            out.entry(edge.from.clone()).or_default().push(index);
            if edge.to_port.is_none() {
                let node = &document.nodes[&edge.to];
                let operation = components[&edge.to]
                    .operation(&node.operation)
                    .expect("resolved node operation exists");
                let port = resolve_input_port(&edge.to, &node.operation, None, operation)?;
                inferred_ports.insert(index, port.name.clone());
            }
        }
        let mut input_schemas = HashMap::new();
        let mut output_schemas = HashMap::new();
        for (node_id, node) in &document.nodes {
            let component = components
                .get(node_id)
                .with_context(|| format!("node {node_id:?} has no admitted component"))?;
            let operation = component
                .operation(&node.operation)
                .with_context(|| format!("node {node_id:?} has no admitted operation"))?;
            let operation =
                wamn_catalog::specialize_wiring_operation(node_id, node, component, operation)?;
            let mut ports = HashMap::new();
            for port in &operation.input_ports {
                ports.insert(port.name.clone(), compile_schema(&port.schema.schema)?);
            }
            input_schemas.insert(node_id.clone(), ports);
            if matches!(operation, Cow::Owned(_)) {
                output_schemas.insert(
                    node_id.clone(),
                    compile_schema(&node.params["output-schema"])?,
                );
            }
        }
        Ok(Self {
            document,
            out,
            inferred_ports,
            input_schemas,
            output_schemas,
        })
    }

    pub(super) fn validates_output(&self, node: &str) -> bool {
        self.output_schemas.contains_key(node)
    }

    pub(super) fn validate_output(
        &self,
        node: &str,
        payload: &serde_json::Value,
    ) -> Result<(), super::ErrorDetail> {
        let Some(schema) = self.output_schemas.get(node) else {
            return Ok(());
        };
        schema
            .schemas
            .validate(payload, schema.index)
            .map_err(|error| {
                super::ErrorDetail::coded(
                    "schema-invalid",
                    format!("node {node:?} output does not match its declared schema: {error}"),
                )
            })
    }

    pub(super) fn validate_input(
        &self,
        node: &str,
        port: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<(), super::ErrorDetail> {
        let ports = &self.input_schemas[node];
        let selected = port.and_then(|name| ports.get(name)).or_else(|| {
            if port.is_none() && ports.len() == 1 {
                ports.values().next()
            } else if port.is_none() {
                ports.get("input")
            } else {
                None
            }
        });
        let Some(schema) = selected else {
            return Err(super::ErrorDetail::coded(
                "invalid-input-port",
                "no target input port",
            ));
        };
        schema
            .schemas
            .validate(payload, schema.index)
            .map_err(|error| {
                super::ErrorDetail::coded(
                    "schema-invalid",
                    format!("node {node:?} input does not match its port schema: {error}"),
                )
            })
    }

    pub(super) fn entry(&self) -> &str {
        &self.document.entry
    }
    pub(super) fn node(&self, id: &str) -> Option<&WiringNode> {
        self.document.nodes.get(id)
    }
    pub(super) fn successors<'a>(
        &'a self,
        node: &str,
        port: &'a str,
    ) -> impl Iterator<Item = (&'a WiringEdge, &'a str)> {
        self.out
            .get(node)
            .into_iter()
            .flatten()
            .filter_map(move |index| {
                let edge = &self.document.edges[*index];
                (edge.from_port == port).then(|| {
                    (
                        edge,
                        edge.to_port
                            .as_deref()
                            .unwrap_or_else(|| self.inferred_ports[index].as_str()),
                    )
                })
            })
    }
}

fn compile_schema(schema: &serde_json::Value) -> anyhow::Result<PortSchema> {
    let uri = "mem://wamn-port.json";
    let mut schemas = Schemas::new();
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    compiler
        .add_resource(uri, schema.clone())
        .map_err(|error| anyhow::anyhow!("port schema: {error}"))?;
    let index = compiler
        .compile(uri, &mut schemas)
        .map_err(|error| anyhow::anyhow!("port schema: {error}"))?;
    Ok(PortSchema { schemas, index })
}

impl std::fmt::Debug for Wiring {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Wiring")
            .field("document", &self.document)
            .finish_non_exhaustive()
    }
}
