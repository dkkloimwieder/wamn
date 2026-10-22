"""
The project document declares the GCP cluster and its node pools.
"""
import regex

_name = r"^[a-z][a-z0-9-]*[a-z0-9]$|^[a-z]$"

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

schema Edge:
    # The GKE Service owns this NEG. OpenTofu only reads it.
    neg_name: "wamn-edge"
    neg_zone: str

    check:
        regex.match(neg_zone, r"^[a-z]+-[a-z]+[0-9]+-[a-z]$"), "neg_zone is a GCP zone"

schema Project:
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
    edge: Edge

    check:
        len(cluster_name) <= 40 and regex.match(cluster_name, _name), "cluster_name is a lowercase name of at most 40 characters"
        len(node_pools) > 0 and len({pool.name: True for pool in node_pools}) == len(node_pools), "node pools are nonempty with distinct names"
        regex.match(project_id, r"^[a-z][a-z0-9-]{4,28}[a-z0-9]$"), "project_id is a GCP project id"
        regex.match(zone, r"^[a-z]+-[a-z]+[0-9]+-[a-z]$"), "zone is a GCP zone"
        release_channel in ["rapid", "regular", "stable", "extended"], "release_channel is a named channel"
        all name in [network, subnetwork, pod_secondary_range_name, service_secondary_range_name] {len(name) <= 63 and regex.match(name, _name)}, "network and range names are lowercase names"
        regex.match(node_service_account, r"^[a-z][a-z0-9-]*@[a-z][a-z0-9-]*\.iam\.gserviceaccount\.com$"), "node_service_account is a GCP service account email"
