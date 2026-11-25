"""
The schema module of a package manifest (docs/plan/manifest-authoring.md).

A package's wamn.k imports this module, and the generator compiles wamn.k to
generated/wamn.json. Each schema lists its keys in the order of the manifest
reader, crates/codegen/src/manifest.rs. A default here is an authoring default: it fills a value that the
author did not write, and the compiled JSON states it. An author who sets an
optional key to None leaves the key out of the compiled JSON.

The author states a fact once (docs/architecture/data-access.md#manifest-declarations). The SQL
owns what a statement reads and writes, so no schema here has a relations key:
the generator derives each custom operation's relations from its statements
and writes them into generated/wamn.json. A command's envelope bounds and
input bounds are defaults, written only to change them, and the detail of
idempotency_conflict is derived with the other platform error details. The
schema owns column constraints and stamps: a model's enum_fields come from its
CHECK col IN ('a', ...) constraints, and its stamp columns are server-owned
without being listed. A create operation can narrow a column's allowed values.
A model query that one table answers
states its sort and filters, and the generator writes its SQL.
"""


type Direction = "ascending" | "descending"
type ResultClass = "one" | "optional_one" | "page" | "bounded_list"
type Fetch = "one" | "optional_one" | "bounded_list"
type Visibility = "public" | "private"
type Transaction = "explicit_per_input"
type Match = "exact" | "contains" | "prefix" | "range" | "is_null"
type HistoryColumn = "created_at" | "created_by" | "updated_at" | "updated_by"
type EventOp = "insert" | "update" | "delete"
# A target type a package runs on.
type Target = "cloud" | "local"
# A column role a model states.
type FieldRole = "actor"

schema PackageIdentity:
    id: str
    version: str

schema ClientPackage:
    name: str

schema BaseDependency:
    package: str
    version: str
    digest: str
    operations: [str]

schema PolicyContract:
    id: str
    state: "unsatisfied" | "satisfied"

schema Ref:
    model: str
    narrowed_by?: str

schema FieldLabel:
    label?: str
    description?: str
    references?: Ref

schema CountLimit:
    minimum: int
    maximum: int
    label?: str
    description?: str

schema Input:
    raw_body_maximum?: int
    envelope?: CountLimit
    line?: CountLimit
    key?: str
    request_id?: str
    labels?: {str:FieldLabel}
    revision_guards?: {str:str}

schema SignedRead:
    lifetime_seconds?: int

schema Result:
    signed_reads?: {str:SignedRead}
    labels?: {str:FieldLabel}

schema Statement:
    fetch: Fetch
    binds: [str]

schema StateGuard:
    guards: {str:str}

schema State:
    state: StateGuard

schema List:
    model?: str
    key_field: str | [str]
    display_field?: str

schema CustomOperation[name: str]:
    """A custom operation. Its permission defaults to its own name."""
    type: "projection" | "command" | "event_handler"
    visibility: Visibility = "public"
    append_only?: bool
    permission?: str = name
    component?: str
    connection?: str = "postgres"
    input: Input = {}
    result?: Result
    error_text?: {str:str}
    # A constraint the SQL can violate, mapped to the business error it answers.
    constraint_errors?: {str:str}
    statements?: {str:Statement}
    transaction?: Transaction
    automatic_retry?: bool
    idempotent_by?: "claim" | "stateless" | State
    label?: str
    description?: str
    lists?: List

# Whether a command claims its key. Such a command takes the public envelope: a
# batch of keyed inputs under one raw body bound.
_claims = lambda idempotent_by: any -> bool {
    typeof(idempotent_by) == "str" and idempotent_by == "claim"
}

schema Command[name: str](CustomOperation):
    """A command. One that takes the public envelope gets the platform's bounds,
    a raw body of at most 1 MiB and 1 to 100 inputs, and one that claims its key
    leaves the request id and the key out of its canonical input. Merge the input
    with the union operator, input: {...}, so that the command fills its bounds."""
    type = "command"
    transaction = "explicit_per_input"
    automatic_retry = False

    if _claims(idempotent_by) and not input.raw_body_maximum:
        input.raw_body_maximum = 1048576
    if _claims(idempotent_by) and not input.envelope:
        input.envelope = {minimum = 1, maximum = 100}

schema Projection[name: str](CustomOperation):
    type = "projection"
    feed?: bool
    sync?: bool

schema EventHandler[name: str](CustomOperation):
    type = "event_handler"

schema AuthoredSqlVariant:
    field: str
    direction: Direction
    path: str

schema AuthoredSql:
    default: str
    variants: [AuthoredSqlVariant]

schema LastDays:
    last_days: int

schema Filter:
    field: str
    match?: Match
    required?: bool
    default?: LastDays

schema Search:
    fields: [str]

schema Sort:
    fields: [str]
    directions: [Direction]

schema SortKey:
    field: str
    direction: Direction

schema TieBreaker:
    field: str

schema Pagination:
    default_sort: SortKey
    tie_breaker: TieBreaker

schema Limit:
    default: int
    minimum: int
    maximum: int

schema Operation:
    """A get, query, create, update or delete operation of a model. The model fills
    its permission with <model>.<operation>."""
    permission?: str
    append_only?: bool
    component?: str
    authored_sql?: AuthoredSql
    writable_fields?: [str]
    values?: {str:[str]}
    revision_field?: str
    filters?: [Filter]
    search?: Search
    sort?: Sort
    pagination?: Pagination
    limit?: Limit
    label?: str
    description?: str
    result: ResultClass

schema Get(Operation):
    """The get operation of a model."""

schema Query(Operation):
    feed?: bool
    display_field?: str
    pagination = Pagination {
        default_sort = {field = "created_at", direction = "ascending"}
        tie_breaker = {field = "id"}
    }
    limit = Limit {default = 100, minimum = 1, maximum = 100}

schema Create(Operation):
    """The create operation of a model."""

schema Update(Operation):
    """The update operation of a model."""

schema Delete(Operation):
    """The delete operation of a model."""

schema Operations:
    get?: Get
    query?: Query
    create?: Create
    update?: Update
    delete?: Delete

schema AuditLog:
    columns: [HistoryColumn]
    retention: str = "none"

schema TextOfField:
    label?: str
    description?: str

schema ModelField:
    """What fills one column: number names the sequence the generated insert
    numbers it from, and role = "actor" fills it from the operation caller."""
    number?: str
    role?: FieldRole

schema Model[name: str, schema_name: str]:
    """A model. Model("widget", "fixture") names the model and the package's schema.
    Merge the operations with the union operator, operations: {get: {...}}: the
    model then fills the get permission with widget.get. An = in place of a :
    replaces the operation after the model fills it."""
    $schema: str = schema_name
    table: str
    owner: str
    client_field_extensible?: bool
    field_owners?: {str:str}
    constraint_owners?: {str:str}
    # Server-owned fields other than the stamps, which come from the table.
    server_owned_fields?: [str]
    # Only a narrower set than the column's CHECK, or a set no CHECK carries.
    min_lengths?: {str:int}
    field_text?: {str:TextOfField}
    fields?: {str:ModelField}
    audit_log?: AuditLog
    delete_mode?: "hard" | "tombstone"
    operations: Operations

    if operations and operations.get and not operations.get.permission:
        operations.get.permission = "${name}.get"
    if operations and operations.query and not operations.query.permission:
        operations.query.permission = "${name}.query"
    if operations and operations.create and not operations.create.permission:
        operations.create.permission = "${name}.create"
    if operations and operations.update and not operations.update.permission:
        operations.update.permission = "${name}.update"
    if operations and operations.delete and not operations.delete.permission:
        operations.delete.permission = "${name}.delete"

schema Sequence:
    """A number sequence and its default pattern. Any command of the package
    calls it through sequences::<name>::next."""
    pattern: str

schema InternalRelation:
    $schema: str
    table: str
    cdc: "excluded"

schema DataChange:
    role: str
    models: {str:[EventOp]}
    host_transaction: bool = True

schema Component:
    data_change?: DataChange
    connections?: [str]
    exports?: {str:any}
    parameters?: {str:[any]}

schema HttpExposure:
    id: str
    path: str

schema Routes:
    operations?: {str:HttpExposure}
    auth_modes?: [str]
    path_prefix?: str
    id_prefix?: str

schema Connection:
    """An external connection requirement. Deployment supplies the named row."""
    interface: str
    mode: "read" | "write"


schema Package:
    """The whole manifest. A package's wamn.k is one Package instance."""
    package: PackageIdentity
    client_package?: ClientPackage
    base_dependencies?: {str:BaseDependency}
    required_platform_policy_contract: PolicyContract
    # The target types the package runs on; cloud when unset.
    targets?: [Target]
    sequences?: {str:Sequence}
    models: {str:Model}
    internal_relations?: {str:InternalRelation}
    custom_operations?: {str:CustomOperation}
    connections: {str:Connection} = {}
    components: {str:Component}
    routes?: Routes

    check:
        # A model tests nothing here: an operation assigned with = replaces
        # the model's fill only after the model's own tests run.
        all _, model in models {
            all op in ["get", "query", "create", "update", "delete"] {
                not model.operations[op] or model.operations[op].permission
            }
        }, "a model operation has no permission: merge the operations with ':', operations: {get: {...}}, so that the model fills each permission"
        # The same holds for a command's input and its envelope bounds.
        not custom_operations or all _, operation in custom_operations {
            operation.type != "command" or not _claims(operation.idempotent_by) or (operation.input.raw_body_maximum and operation.input.envelope)
        }, "a command that takes the public envelope has no bounds: merge the input with ':', input: {...}, so that the command fills them"
