import environment

environment.Environment {
    org = "dkk"
    project = "wms"
    env = "dev"
    release = "none"
    route_host = "wms.wamn.dev"
    policy = "dev"
    connections = {
        "labels" = {type = "blobstore", definition = {"provider": "gcs", "container": "wamn-dev-labels", "prefix": "wms/"}}
    }
    floors = {
    }
}
