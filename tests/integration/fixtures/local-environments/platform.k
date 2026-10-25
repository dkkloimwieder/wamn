import environment

environment.Environment {
    org = "acme"
    project = "platform"
    env = "dev"
    release = "none"
    route_host = "platform.localhost"
    policy = "dev"
    applications = {orders = {packages = ["platform_fixture"]}}
    connections = {}
    floors = {}
}
