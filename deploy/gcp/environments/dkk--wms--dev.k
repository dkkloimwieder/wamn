import environment

environment.Environment {
    org = "dkk"
    project = "wms"
    env = "dev"
    release = "sha256:d31830b104ace4a09180b63e7dcfe5152a3a59db093553e5bbf67cda65adeec9"
    route_host = "wms.wamn.dev"
    policy = "dev"
    connections = {
        "labels" = {type = "blobstore", definition = {"provider": "gcs", "container": "wamn-dev-labels", "prefix": "wms/"}}
    }
    floors = {
    }
}
