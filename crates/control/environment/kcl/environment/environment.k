"""Applied environment configuration. Releases are selected through deploy."""
import regex

schema Client:
    package: str
    web: str

schema Application:
    hostname: str
    revocation_seconds: int
    client?: Client
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

    _invalid_client = [name for name, app in applications if app.client != Undefined and app.client != None and not regex.match(app.client.web, r"^sha256:[0-9a-f]{64}$")]

    check:
        revocation_seconds >= 0, "revocation_seconds must be nonnegative"
        len(_invalid_client) == 0, "invalid client web digest for applications ${_invalid_client}"
        len(target) > 0, "target is required"
