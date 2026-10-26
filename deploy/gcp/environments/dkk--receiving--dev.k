import environment

environment.Environment {
    target = "cloud"
    org = "dkk"
    project = "receiving"
    env = "dev"
    release = "sha256:2bf64b612d4e69acbe68cac2da55f66e1bbc0f41e994fa8874a97d0f815210e0"
    route_host = "receiving.wamn.dev"
    policy = "dev"
    applications = {
        "wamn_receiving" = {packages = ["wamn_receiving"]}
    }
    floors = {
    }
}
