import environment

environment.Environment {
    target = "cloud"
    org = "dkk"
    project = "wms"
    env = "dev"
    release = "sha256:05fa8fc12eca4baf25c62dbabb5453a943f9767115241dc2c94c6d23c79b2bd3"
    route_host = "wms.wamn.dev"
    policy = "dev"
    integrations = {
        "wms-labels" = {version = 1}
        "rest-fixture" = {version = 1}
    }
    applications = {
        "wamn_wms" = {packages = ["wamn_wms"], connections = {labels = "wms-labels"}}
        rest = {packages = ["rest_connector", "rest_consumer_fixture", "wamn_integration"], connections = {upstream = "rest-fixture"}}
    }
    floors = {
    }
}
