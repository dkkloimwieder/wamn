import environment

environment.Environment {
    target = "cloud"
    org = "acme"
    project = "platform"
    env = "dev"
    release = "none"
    route_host = "platform.localhost"
    policy = "dev"
    applications = {orders = {packages = ["platform_fixture"]}}
    floors = {}
}
