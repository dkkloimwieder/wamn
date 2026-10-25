import environment

environment.Environment {
    org = "acme"
    project = "receiving"
    env = "dev"
    release = "none"
    route_host = "receiving.localhost"
    policy = "dev"
    applications = {receiving = {packages = ["wamn_receiving", "client_acme_receiving"]}}
    connections = {}
    floors = {}
}
