//! Application integration tests.

#[cfg(test)]
mod wms_publication;
#[cfg(test)]
#[cfg_attr(
    not(feature = "cluster"),
    expect(
        dead_code,
        reason = "the cluster tests use these helpers, and a build without the cluster feature leaves them out; the clippy run with the feature checks them"
    )
)]
mod wms_runtime_live;
#[cfg(test)]
mod wms_wiring_shape;

#[cfg(all(test, feature = "cluster"))]
mod cluster;
#[cfg(test)]
#[cfg_attr(
    not(feature = "cluster"),
    expect(
        dead_code,
        unused_imports,
        reason = "the cluster tests use these helpers, and a build without the cluster feature leaves them out; the clippy run with the feature checks them"
    )
)]
mod delivery;
#[cfg(test)]
#[cfg_attr(
    not(feature = "cluster"),
    expect(
        dead_code,
        reason = "the cluster tests use these helpers, and a build without the cluster feature leaves them out; the clippy run with the feature checks them"
    )
)]
mod environment;

#[cfg(test)]
mod business_fixture;

#[cfg(test)]
mod local_business;
