/// The platform image set for the host and application workload guests.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageSet {
    /// `<repository>:<tag>@sha256:<hex>`.
    pub host: String,
    /// `<repository>@sha256:<hex>`.
    pub http: String,
    /// `<repository>@sha256:<hex>`.
    pub workflow: String,
}
