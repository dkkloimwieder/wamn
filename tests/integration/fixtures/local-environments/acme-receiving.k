import environment

environment.Environment {
    target = "cloud"
    org = "acme"
    project = "receiving"
    env = "dev"
    policy = "dev"
    applications = {receiving = {current = "none", hostname = "receiving.localhost", packages = ["wamn_receiving", "client_acme_receiving"]}}
}
