import environment

environment.Environment {
    target = "cloud"
    org = "acme"
    project = "receiving"
    env = "dev"
    policy = "dev"
    revocation_seconds = 60
    applications = {receiving = {current = "none", revocation_seconds = 5, hostname = "receiving.localhost", packages = ["wamn_receiving"]}}
}
