//! Index the catalog graph without copying its nodes or edges.
use std::collections::{BTreeMap, HashMap};
use wamn_catalog::{AdmittedComponent, WiringDocument, WiringEdge, WiringNode, resolve_input_port};

pub(super) const DEFAULT_HOP_LIMIT: u64 = 10_000;

#[derive(Debug)]
pub(crate) struct Wiring {
    document: WiringDocument,
    out: HashMap<String, Vec<usize>>,
    inferred_ports: HashMap<usize, String>,
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
        Ok(Self {
            document,
            out,
            inferred_ports,
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
