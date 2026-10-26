import environment

environment.Environment {
    target = "cloud"
    org = "acme"
    project = "receiving"
    env = "dev"
    release = "none"
    route_host = "receiving.localhost"
    policy = "dev"
    applications = {receiving = {packages = ["wamn_receiving", "client_acme_receiving"]}}
    floors = {}
}
