import cluster

cluster.Cluster {
    provider = "gcp"
    project_id = "wamn-dev"
    zone = "us-central1-a"
    cluster_name = "wamn"
    org = "dkk"
    owner_email = "dkkloimwieder@gmail.com"
    registry = "us-central1-docker.pkg.dev/wamn-dev/wamn"
    buckets = {web = "wamn-dev-web", otel = "wamn-dev-otel", state = "wamn-dev-tofu-state"}
    route_hosts = {
        control = "control.wamn.dev"
    }
    resend_from = "noreply@wamn.dev"
}
