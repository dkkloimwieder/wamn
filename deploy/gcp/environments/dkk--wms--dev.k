import environment

environment.Environment {
    org = "dkk"
    project = "wms"
    env = "dev"
    release = "sha256:354f27f9535daf3ecd5f3568d09ae44a966fa8f37480db98ed49efb7bc9ebe0e"
    route_host = "wms.wamn.dev"
    policy = "dev"
    applications = {
        "wamn_wms" = {packages = ["wamn_wms"]}
    }
    connections = {
        "labels" = {type = "blobstore", definition = {"provider": "gcs", "container": "wamn-dev-labels", "prefix": "wms/"}}
    }
    floors = {
    }
}
