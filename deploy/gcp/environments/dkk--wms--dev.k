import environment

environment.Environment {
    target = "cloud"
    org = "dkk"
    project = "wms"
    env = "dev"
    release = "sha256:354f27f9535daf3ecd5f3568d09ae44a966fa8f37480db98ed49efb7bc9ebe0e"
    route_host = "wms.wamn.dev"
    policy = "dev"
    applications = {
        "wamn_wms" = {packages = ["wamn_wms"], connections = {labels = "wms-labels"}}
        rest = {packages = ["rest_connector", "rest_consumer_fixture", "wamn_integration"], connections = {upstream = "rest-fixture"}}
    }
    floors = {
    }
}
