import environment

environment.Environment {
    org = "acme"
    project = "receiving"
    env = "dev"
    release = "none"
    route_host = "receiving.localhost"
    policy = "dev"
    applications = {receiving = {packages = ["wamn_receiving", "wamn_dock"]}}
    connections = {}
    floors = {}
}
