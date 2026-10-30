"""
The schema module of an environment document (docs/plan/platform-deploy.md §10.1, R21).

An environment file imports this module, and `wamn-ctl env apply` compiles it
to canonical JSON. The schema has exactly the fields R21 names: the
coordinate, the release digest or "none", the route host, the policy name, the
applications (docs/plan/platform-config.md), the target, the connection names and
the floors.
"""
import regex

schema Application:
    """An application of the environment: the packages of the release that belong to it."""
    packages: [str]
    connections: {str:str} = {}

schema Integration:
    version: int

    check:
        version > 0, "version must be positive"

schema Environment:
    org: str
    project: str
    env: str
    target: str
    # A published, qualified release by its full digest, or "none".
    release: str
    route_host: str
    # Package id of the web client for route_host; one client defaults itself.
    client_package?: str
    # A policy of registry.env_policies, by name.
    policy: str
    # Application name to its packages. Every package of the release belongs
    # to exactly one application.
    applications: {str:Application}
    integrations: {str:Integration} = {}
    # The package versions to contract to: package id to version (R13).
    floors: {str:str} = {}

    check:
        len(target) > 0, "target is required"
        release == "none" or regex.match(release, r"^sha256:[0-9a-f]{64}$"), "release is a full sha256: digest, or none"
