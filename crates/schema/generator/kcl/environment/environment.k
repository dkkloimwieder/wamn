"""Shared environment configuration and each application's release selection."""
import regex

schema Client:
    package: str
    web: str

schema Application:
    current: str
    previous?: str
    hostname: str
    revocation_seconds: int
    client?: Client
    packages: [str]
    connections: {str:str} = {}

    check:
        revocation_seconds >= 0, "revocation_seconds must be nonnegative"

schema Integration:
    version: int

    check:
        version > 0, "version must be positive"

schema Schema:
    mode?: "reset" | "migrate"

schema Environment:
    org: str
    project: str
    env: str
    target: str
    policy: str
    revocation_seconds: int
    schema: Schema = {}
    applications: {str:Application}
    integrations: {str:Integration} = {}

    _invalid_current = [name for name, app in applications if app.current != "none" and not regex.match(app.current, r"^sha256:[0-9a-f]{64}$")]
    _invalid_previous = [name for name, app in applications if app.previous != Undefined and app.previous != None and not regex.match(app.previous, r"^sha256:[0-9a-f]{64}$")]
    _invalid_client = [name for name, app in applications if app.client != Undefined and app.client != None and not regex.match(app.client.web, r"^sha256:[0-9a-f]{64}$")]

    check:
        revocation_seconds >= 0, "revocation_seconds must be nonnegative"
        len(_invalid_current) == 0, "invalid current release for applications ${_invalid_current}"
        len(_invalid_previous) == 0, "invalid previous release for applications ${_invalid_previous}"
        len(_invalid_client) == 0, "invalid client web digest for applications ${_invalid_client}"
        len(target) > 0, "target is required"
