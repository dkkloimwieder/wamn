import environment

environment.Environment {
    target = "cloud"
    org = "dkk"
    project = "wms"
    env = "dev"
    policy = "dev"
    revocation_seconds = 60
    integrations = {
        "wms-labels" = {version = 1}
    }
    applications = {
        "wamn_wms" = {revocation_seconds = 5, hostname = "wms.wamn.dev", connections = {labels = "wms-labels"}}
    }
}
