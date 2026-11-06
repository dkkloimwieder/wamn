import environment

environment.Environment {
    target = "cloud"
    org = "dkk"
    project = "receiving"
    env = "dev"
    policy = "dev"
    integrations = {}
    applications = {
        "wamn_receiving" = {current = "sha256:05c000d9ed875a4fcedfd8491d6367a84bc4b9f35f52ba382aba8c995f8f9575", hostname = "receiving.wamn.dev", packages = ["wamn_receiving"]}
    }
}
