"""
The schema module of an environment document (docs/plan/platform-deploy.md §10.1, R21).

An environment file imports this module, and `wamn-ctl env apply` compiles it
to canonical JSON. The schema has exactly the fields R21 names: the
coordinate, the release digest or "none", the route host, the policy name, the
applications (docs/plan/platform-config.md), the connection definitions and
the floors.
"""
import regex

schema Connection:
    """A connection definition. Credentials are provisioned, never declared."""
    type?: "blobstore" | "http"
    definition?: {str:any}
    org?: str

    check:
        (org != Undefined and type == Undefined and definition == Undefined) or (org == Undefined and type != Undefined and definition != Undefined), "use an org reference or an inline definition"

schema Application:
    """An application of the environment: the packages of the release that belong to it."""
    packages: [str]

schema Environment:
    org: str
    project: str
    env: str
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
    connections: {str:Connection} = {}
    # The package versions to contract to: package id to version (R13).
    floors: {str:str} = {}

    check:
        release == "none" or regex.match(release, r"^sha256:[0-9a-f]{64}$"), "release is a full sha256: digest, or none"
