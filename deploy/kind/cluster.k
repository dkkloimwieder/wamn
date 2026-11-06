import cluster

cluster.Cluster {
    provider = "kind"
    project_id = ""
    zone = ""
    cluster_name = "wamn-local"
    org = "local"
    owner_email = "owner@example.com"
    registry = "localhost:5001"
    buckets = {web = "", otel = "", state = ""}
    route_hosts = {
        control = "control.localhost"
    }
    resend_from = "noreply@example.com"
}
