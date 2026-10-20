"""
The schema module of a cluster document (docs/plan/platform-install.md P1, P3).

A cluster file imports this module. The schema names only the install inputs
of P1. Owner resources are read by name, never created from this document.
"""
import regex

_name = r"^[a-z][a-z0-9-]*[a-z0-9]$|^[a-z]$"
_host = r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$"
_email = r"^[^\s@]+@[^\s@]+\.[^\s@]+$"

schema Buckets:
    web: str
    otel: str

schema RouteHosts:
    control: str

    check:
        len(control) <= 253 and regex.match(control, _host), "control is a lowercase DNS name"

schema Cluster:
    provider: "gcp" | "kind"
    # GCP-only inputs are empty strings for kind.
    project_id: str
    zone: str
    cluster_name: str
    org: str
    owner_email: str
    registry: str
    buckets: Buckets
    route_hosts: RouteHosts
    resend_from: str

    check:
        len(cluster_name) <= 40 and regex.match(cluster_name, _name), "cluster_name is a lowercase name of at most 40 characters"
        regex.match(org, r"^[a-z][a-z0-9_-]*$"), "org is a coordinate name"
        regex.match(owner_email, _email), "owner_email is an email address"
        regex.match(resend_from, _email), "resend_from is an email address"
        regex.match(registry, r"^[a-z0-9][a-z0-9.:-]*(/[a-z0-9][a-z0-9._-]*)*$"), "registry is a registry host and optional repository path"
        provider == "kind" or regex.match(project_id, r"^[a-z][a-z0-9-]{4,28}[a-z0-9]$"), "project_id is a GCP project id"
        provider == "kind" or regex.match(zone, r"^[a-z]+-[a-z]+[0-9]+-[a-z]$"), "zone is a GCP zone"
        provider == "kind" or all bucket in [buckets.web, buckets.otel] {len(bucket) >= 3 and len(bucket) <= 63 and regex.match(bucket, _host)}, "buckets are lowercase bucket names"
