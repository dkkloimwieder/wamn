import environment

environment.Environment {
    target = "cloud"
    org = "acme"
    project = "wms"
    env = "dev"
    release = "none"
    route_host = "wms.localhost"
    policy = "dev"
    applications = {warehouse = {packages = ["wamn_wms"]}}
    floors = {}
}
