/// The platform image set: the host image and the role component images.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageSet {
    /// `<repository>:<tag>@sha256:<hex>`.
    pub host: String,
    /// `<repository>@sha256:<hex>`.
    pub http: String,
}
