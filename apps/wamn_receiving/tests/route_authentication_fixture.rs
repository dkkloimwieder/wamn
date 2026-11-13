use command_histories::database::bind_fixture_principal;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::Context;
use tokio::process::Command;
use tokio_postgres::Client;
use wamn_dev_control::dev::watch::GitSource;
use wamn_dev_control::git_source::GitSourceState;

use environment::{
    journey_package_root, overlay_package_root, package_root, repository_root, required_journey,
    required_journey_path, verify_journey_operation_grants,
};
use routes::copy_package;
use wamn_dev_control::dev::environment::{
    DevEnvironmentInputs, ENVIRONMENT, ORG, PROJECT, connect, install_journey_platform_floor,
    write_dev_config,
};
use wamn_environment_control::provision_project_env::read_json;
use wamn_release_control::delivery::ReleaseIdentity;
use wamn_test_infrastructure::operations::package_version;
use wamn_test_infrastructure::scratch::ScratchRoot;

const ADMIN_ROLE: &str = "admin";
const BASE_PACKAGE_ID: &str = "wamn_receiving";
/// The version of the base package, from its wamn.json.
static BASE_PACKAGE_VERSION: LazyLock<&'static str> =
    LazyLock::new(|| package_version(BASE_PACKAGE_ID).leak());
const BASE_COMPONENT: &str = "receiving";
const OVERLAY_PACKAGE_ID: &str = "client_acme_receiving";
/// The version of the overlay package, from its wamn.json.
static OVERLAY_PACKAGE_VERSION: LazyLock<&'static str> =
    LazyLock::new(|| package_version(OVERLAY_PACKAGE_ID).leak());
const OVERLAY_COMPONENT: &str = "client_acme_receiving";
/// Domain of the platform principal emails in every test tenant.
const PLATFORM_DOMAIN: &str = "example.invalid";

/// The Receiving release from this checkout.
fn identity() -> &'static ReleaseIdentity {
    static IDENTITY: LazyLock<ReleaseIdentity> = LazyLock::new(|| ReleaseIdentity {
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        environment: ENVIRONMENT.to_owned(),
        route_host: "receiving.localhost".to_owned(),
        packages: JOURNEY_PACKAGES
            .iter()
            .map(|package| wamn_catalog::PackageCoordinate::new(package.id, package.version()))
            .collect::<Result<_, _>>()
            .expect("read the Receiving journey package coordinates"),
    });
    &IDENTITY
}

/// The journey packages that the release of [`identity`] carries.
fn released_journey_packages() -> impl Iterator<Item = JourneyPackage> {
    JOURNEY_PACKAGES.into_iter().filter(|package| {
        identity()
            .packages
            .iter()
            .any(|coordinate| coordinate.package_id() == package.id)
    })
}

/// The root of each released journey package, by component.
fn released_package_roots() -> BTreeMap<&'static str, PathBuf> {
    released_journey_packages()
        .map(|package| (package.component, journey_package_root(package)))
        .collect()
}

#[derive(Clone, Copy)]
struct JourneyPackage {
    id: &'static str,
    component: &'static str,
}

impl JourneyPackage {
    /// The version of the package, from its wamn.json.
    fn version(&self) -> &'static str {
        if self.id == BASE_PACKAGE_ID {
            *BASE_PACKAGE_VERSION
        } else {
            *OVERLAY_PACKAGE_VERSION
        }
    }
}

const JOURNEY_PACKAGES: [JourneyPackage; 2] = [
    JourneyPackage {
        id: BASE_PACKAGE_ID,
        component: BASE_COMPONENT,
    },
    JourneyPackage {
        id: OVERLAY_PACKAGE_ID,
        component: OVERLAY_COMPONENT,
    },
];
