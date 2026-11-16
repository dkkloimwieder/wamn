use database::bind_fixture_principal;

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Context;

use environment::{overlay_package_root, package_root, repository_root};
use routes::copy_package;
use wamn_postgres::connect;
const ORG: &str = "acme";
const PROJECT: &str = "receiving";
const ENVIRONMENT: &str = "dev";

use wamn_release_control::delivery::ReleaseIdentity;
use wamn_test_infrastructure::operations::package_version;
use wamn_test_infrastructure::scratch::ScratchRoot;

const BASE_PACKAGE_ID: &str = "wamn_receiving";
/// The version of the base package, from its wamn.json.
static BASE_PACKAGE_VERSION: LazyLock<wamn_catalog::package_version::PackageVersion> =
    LazyLock::new(|| package_version(&BASE_PACKAGE_ID.parse().expect("valid fixture name")));
const OVERLAY_PACKAGE_ID: &str = "client_acme_receiving";
/// The version of the overlay package, from its wamn.json.
static OVERLAY_PACKAGE_VERSION: LazyLock<wamn_catalog::package_version::PackageVersion> =
    LazyLock::new(|| package_version(&OVERLAY_PACKAGE_ID.parse().expect("valid fixture name")));

/// The Receiving release from this checkout.
fn identity() -> &'static ReleaseIdentity {
    static IDENTITY: LazyLock<ReleaseIdentity> = LazyLock::new(|| ReleaseIdentity {
        org: ORG.parse().unwrap(),
        project: PROJECT.parse().unwrap(),
        environment: ENVIRONMENT.parse().unwrap(),
        route_host: "receiving.localhost".to_owned(),
        packages: JOURNEY_PACKAGES
            .iter()
            .map(|package| {
                wamn_catalog::PackageCoordinate::new(
                    package.id.parse().unwrap(),
                    package.version().clone(),
                )
            })
            .collect::<Result<_, _>>()
            .expect("read the Receiving journey package coordinates"),
    });
    &IDENTITY
}

#[derive(Clone, Copy)]
struct JourneyPackage {
    id: &'static str,
}

impl JourneyPackage {
    /// The version of the package, from its wamn.json.
    fn version(&self) -> &'static wamn_catalog::package_version::PackageVersion {
        if self.id == BASE_PACKAGE_ID {
            &BASE_PACKAGE_VERSION
        } else {
            &OVERLAY_PACKAGE_VERSION
        }
    }
}

const JOURNEY_PACKAGES: [JourneyPackage; 2] = [
    JourneyPackage {
        id: BASE_PACKAGE_ID,
    },
    JourneyPackage {
        id: OVERLAY_PACKAGE_ID,
    },
];
