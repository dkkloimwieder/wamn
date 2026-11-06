import environment

environment.Environment {
    target = "cloud"
    org = "acme"
    project = "wms"
    env = "dev"
    policy = "dev"
    applications = {warehouse = {current = "none", hostname = "wms.localhost", packages = ["wamn_wms"]}}
}
