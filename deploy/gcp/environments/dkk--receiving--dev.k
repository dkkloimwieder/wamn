import environment

environment.Environment {
    target = "cloud"
    org = "dkk"
    project = "receiving"
    env = "dev"
    policy = "dev"
    revocation_seconds = 60
    integrations = {}
    applications = {
        "wamn_receiving" = {revocation_seconds = 5, hostname = "receiving.wamn.dev"}
    }
}
