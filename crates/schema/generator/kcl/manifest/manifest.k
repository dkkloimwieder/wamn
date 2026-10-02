"""
The schema module of a package manifest (docs/plan/manifest-authoring.md).

A package's wamn.k imports this module, and the generator compiles wamn.k to
generated/wamn.json. Each schema lists its keys in the order of the manifest
reader, crates/schema/generator/src/manifest.rs, and the compiled JSON keeps
that order. A default here is an authoring default: it fills a value that the
author did not write, and the compiled JSON states it. An author who sets an
optional key to None leaves the key out of the compiled JSON.
"""

type ColumnType = "boolean" | "int32" | "int64" | "float64" | "text" | "bytes" | "numeric" | "timestamptz" | "json" | "uuid"
type Direction = "ascending" | "descending"
type ResultClass = "one" | "optional_one" | "page" | "bounded_list"
type Fetch = "one" | "optional_one" | "bounded_list"
type Visibility = "public" | "private"
type Transaction = "explicit_per_input" | "participant"
type Match = "exact" | "contains" | "prefix" | "range" | "is_null"
type ErrorDetailKey = "field" | "id" | "expected_row_version" | "observed_row_version" | "minimum" | "maximum" | "observed" | "constraint" | "operation"
type HistoryColumn = "created_at" | "created_by" | "updated_at" | "updated_by"
type EventOp = "insert" | "update" | "delete"
# A field's revision: true on a result row version, or the input field whose row version it guards.
type Revision = bool | str

schema PackageIdentity:
    id: str
    version: str
    predecessor_version?: str

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

schema Field:
    """An input or result field of an operation contract."""
    path: str
    type: ColumnType
    nullable: bool = False
    revision?: Revision
    values?: [str]
    label?: str
    description?: str
    references?: Ref

schema Value:
    """A parameter or row column of a statement."""
    name: str
    type: ColumnType
    nullable: bool = False

schema CountLimit:
    minimum: int
    maximum: int
    label?: str
    description?: str

schema Input:
    raw_body_maximum?: int
    envelope?: CountLimit
    line?: CountLimit
    fields: [Field]

schema Result:
    class: ResultClass
    fields: [Field]

schema ErrorDetail:
    required?: [ErrorDetailKey]
    optional?: [ErrorDetailKey]
    text?: str

schema Relation:
    $schema: str
    table: str
    select_fields: [str]
    insert_fields: [str]
    update_fields: [str]
    delete?: bool
    lock: bool
    constraints?: [str]

schema Statement[directory: str, name: str]:
    """A statement of a custom operation. Its path defaults to <directory>/<name>.sql,
    for example Statement("command/inventory_move", "lock_packaging") or
    Statement("query", "inventory_aggregate")."""
    path: str = "${directory}/${name}.sql"
    fetch: Fetch
    parameters?: [Value]
    row: [Value]

schema StateGuard:
    guards: {str:str}

schema InheritedClaim:
    base: str
    operation: str

schema State:
    state: StateGuard

schema Inherited:
    inherited: InheritedClaim

schema LineOrder:
    ascending_by: str
    positive_member?: str

schema Canonicalization:
    excluded_fields: [str]
    line_order?: LineOrder

schema Registration:
    source_package: str
    entity: str
    ops: [EventOp]
    condition?: str

schema List:
    model?: str
    key_field: str | [str]
    display_field?: str

schema CustomOperation[name: str]:
    """A custom operation. Its permission defaults to its own name."""
    type: "projection" | "command" | "event_handler"
    visibility: Visibility = "public"
    fresh_only?: bool
    permission?: str = name
    component?: str
    connection?: str = "postgres"
    input: Input
    pre_commit?: Input
    pre_commit_required?: bool
    participant?: str
    result?: Result
    errors: [str]
    error_details?: {str:ErrorDetail}
    constraint_errors?: {str:str}
    relations?: [Relation]
    statements?: {str:Statement}
    transaction?: Transaction
    automatic_retry?: bool
    idempotent_by?: "claim" | "stateless" | State | Inherited
    canonicalization?: Canonicalization
    registration?: Registration
    label?: str
    description?: str
    lists?: List

schema Command[name: str](CustomOperation):
    type = "command"
    transaction = "explicit_per_input"
    automatic_retry = False

schema Projection[name: str](CustomOperation):
    type = "projection"

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
    fresh_only?: bool
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
    server_owned_fields?: [str]
    enum_fields?: {str:[str]}
    min_lengths?: {str:int}
    field_text?: {str:TextOfField}
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

schema InternalRelation:
    $schema: str
    table: str
    cdc: "excluded"

schema Component:
    connections: [str]

schema Routes:
    auth_modes?: [str]
    path_prefix?: str
    id_prefix?: str

schema Workflow:
    wiring: str
    registration: Registration

schema Package:
    """The whole manifest. A package's wamn.k is one Package instance."""
    package: PackageIdentity
    client_package?: ClientPackage
    base_dependencies?: {str:BaseDependency}
    required_platform_policy_contract: PolicyContract
    models: {str:Model}
    internal_relations?: {str:InternalRelation}
    custom_operations?: {str:CustomOperation}
    connections: [str]
    components: {str:Component}
    routes?: Routes
    workflows?: {str:Workflow}

    check:
        # A model checks nothing here: an operation assigned with = replaces
        # the model's fill only after the model's own checks run.
        all _, model in models {
            all op in ["get", "query", "create", "update", "delete"] {
                not model.operations[op] or model.operations[op].permission
            }
        }, "a model operation has no permission: merge the operations with ':', operations: {get: {...}}, so that the model fills each permission"

# Typed constructors of a statement value.
uuid = lambda name: str -> Value {
    Value {name = name, type = "uuid"}
}
text = lambda name: str -> Value {
    Value {name = name, type = "text"}
}
int32 = lambda name: str -> Value {
    Value {name = name, type = "int32"}
}
int64 = lambda name: str -> Value {
    Value {name = name, type = "int64"}
}
json = lambda name: str -> Value {
    Value {name = name, type = "json"}
}
numeric = lambda name: str -> Value {
    Value {name = name, type = "numeric"}
}
timestamptz = lambda name: str -> Value {
    Value {name = name, type = "timestamptz"}
}
nullable = lambda value: Value -> Value {
    Value {name = value.name, type = value.type, nullable = True}
}

# The three inputs the platform supplies to a command.
request_id = Field {path = "request_id", type = "text"}
idempotency_key = Field {path = "value.idempotency_key", type = "text"}
occurred_at = Field {path = "value.occurred_at", type = "timestamptz"}
command_inputs = [request_id, idempotency_key, occurred_at]

# The seven platform error codes, in the order the manifests list them. Business
# errors follow invalid_input.
platform_errors = ["invalid_input", "concurrency_conflict", "idempotency_conflict", "retry", "timeout", "permission_denied", "internal_error"]
# The standard details of a not-found error and of an idempotency conflict.
not_found_details = ErrorDetail {required = ["field", "id"]}
conflict_details = ErrorDetail {required = ["field"]}
