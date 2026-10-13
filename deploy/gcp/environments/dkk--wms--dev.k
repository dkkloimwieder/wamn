import environment

environment.Environment {
    org = "dkk"
    project = "wms"
    env = "dev"
    release = "sha256:47171ec08678806e5722b782070a7fc3221191fb71433136664879c0d02d38c7"
    route_host = "wms.wamn.dev"
    policy = "dev"
    connections = {
        "labels" = {type = "blobstore", definition = {"provider": "gcs", "container": "wamn-dev-labels", "prefix": "wms/"}}
    }
    floors = {
    }
}
