import environment

environment.Environment {
    target = "cloud"
    org = "acme"
    project = "wms"
    env = "dev"
    policy = "dev"
    revocation_seconds = 60
    applications = {warehouse = {current = "none", revocation_seconds = 5, hostname = "wms.localhost", packages = ["wamn_wms"]}}
}
