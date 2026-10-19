"""
The schema module of a cluster document (docs/plan/platform-install.md P1, P3).

A cluster file imports this module. The schema names only the install inputs
of P1. Owner resources are read by name, never created from this document.
"""
import regex

_name = r"^[a-z][a-z0-9-]*[a-z0-9]$|^[a-z]$"
_host = r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$"
_email = r"^[^\s@]+@[^\s@]+\.[^\s@]+$"

schema NodePool:
    name: str
    machine_type: str
    spot: bool
    disk_type: "pd-standard" | "pd-balanced" | "pd-ssd"
    # Disk size in GiB; size is the fixed node count, including zero.
    disk_size: int
    size: int
    # One key=value label and key=value:effect taint, or empty strings.
    label: str = ""
    taint: str = ""

    check:
        len(name) <= 63 and regex.match(name, _name), "name is a lowercase pool name"
        regex.match(machine_type, _name), "machine_type is a machine type name"
        disk_size > 0 and disk_size <= 4294967295, "disk_size is a positive u32"
        size >= 0 and size <= 4294967295, "size is a nonnegative u32"
        label == "" or regex.match(label, r"^[a-z0-9][a-z0-9_.-]*=[a-z0-9][a-z0-9_.-]*$"), "label is key=value or empty"
        taint == "" or regex.match(taint, r"^[a-z0-9][a-z0-9_.-]*=[a-z0-9][a-z0-9_.-]*:(NoSchedule|PreferNoSchedule|NoExecute)$"), "taint is key=value:effect or empty"

schema Buckets:
    web: str
    otel: str

schema RouteHosts:
    control: str
    # Full org/project/env coordinate to route host.
    environments: {str:str}

    check:
        len(control) <= 253 and regex.match(control, _host), "control is a lowercase DNS name"
        all coordinate, host in environments {
            regex.match(coordinate, r"^[a-z][a-z0-9_-]*/[a-z][a-z0-9_-]*/[a-z][a-z0-9_-]*$") and len(host) <= 253 and regex.match(host, _host)
        }, "environments maps coordinates to lowercase DNS names"
        len({host: True for coordinate, host in environments}) == len(environments) and all coordinate, host in environments {host != control}, "route hosts are distinct"

schema Cluster:
    provider: "gcp" | "kind"
    # GCP-only inputs are empty strings for kind.
    project_id: str
    zone: str
    cluster_name: str
    release_channel: str
    network: str
    subnetwork: str
    pod_secondary_range_name: str
    service_secondary_range_name: str
    node_service_account: str
    node_pools: [NodePool]
    platform_version: str
    org: str
    owner_email: str
    registry: str
    buckets: Buckets
    route_hosts: RouteHosts
    resend_from: str

    check:
        len(cluster_name) <= 40 and regex.match(cluster_name, _name), "cluster_name is a lowercase name of at most 40 characters"
        regex.match(org, r"^[a-z][a-z0-9_-]*$"), "org is a coordinate name"
        regex.match(platform_version, r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$"), "platform_version is major.minor.patch"
        regex.match(owner_email, _email), "owner_email is an email address"
        regex.match(resend_from, _email), "resend_from is an email address"
        regex.match(registry, r"^[a-z0-9][a-z0-9.:-]*(/[a-z0-9][a-z0-9._-]*)*$"), "registry is a registry host and optional repository path"
        len(node_pools) > 0 and len({pool.name: True for pool in node_pools}) == len(node_pools), "node pools are nonempty with distinct names"
        all coordinate in route_hosts.environments {coordinate.startswith(org + "/")}, "route coordinates belong to org"
        provider == "kind" or regex.match(project_id, r"^[a-z][a-z0-9-]{4,28}[a-z0-9]$"), "project_id is a GCP project id"
        provider == "kind" or regex.match(zone, r"^[a-z]+-[a-z]+[0-9]+-[a-z]$"), "zone is a GCP zone"
        release_channel in ["rapid", "regular", "stable", "extended"] or (provider == "kind" and release_channel == ""), "release_channel is a named channel"
        provider == "kind" or all name in [network, subnetwork, pod_secondary_range_name, service_secondary_range_name] {len(name) <= 63 and regex.match(name, _name)}, "network and range names are lowercase names"
        provider == "kind" or regex.match(node_service_account, r"^[a-z][a-z0-9-]*@[a-z][a-z0-9-]*\.iam\.gserviceaccount\.com$"), "node_service_account is a GCP service account email"
        provider == "kind" or all bucket in [buckets.web, buckets.otel] {len(bucket) >= 3 and len(bucket) <= 63 and regex.match(bucket, _host)}, "buckets are lowercase bucket names"
