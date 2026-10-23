import environment

environment.Environment {
    org = "dkk"
    project = "wms"
    env = "dev"
    release = "sha256:25398ab981fb7da87a62a17f02ed336d18d1a1335f01723df6efe5b2417b68cf"
    route_host = "wms.wamn.dev"
    policy = "dev"
    connections = {
        "labels" = {type = "blobstore", definition = {"provider": "gcs", "container": "wamn-dev-labels", "prefix": "wms/"}}
    }
    floors = {
    }
}
