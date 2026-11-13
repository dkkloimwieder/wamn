# 03 `wamn-event-wire`

Path `apps/platform/events/wire`. One file, `src/lib.rs`: 536 non-test lines, 346 test lines. No workspace dependencies. 13 crates depend on it. The file was read in full. Every public symbol's callers were searched and the duplicates named below were opened. Nothing was built or run.

## Verdict

- This crate has one topic and the right name: the messages on the event stream and the names of the NATS streams and subjects that carry them. (fact)
- It is not a grab bag. The findings are inside it: hand-written code that a derive would give, one field spelled two ways, name builders copied into another crate, and two defensive steps that cannot fire. (fact for the first three, inference for the last)
- It contains no SQL. (fact)

## What it holds and who calls it

Callers are crates outside this one, test code excluded.

| Part | What it is | Callers |
| --- | --- | --- |
| `Op` | insert, update, delete | 9 crates, including `wamn-router`, whose only use of this crate is `Op` |
| `Causation` | `{run, root, depth}`, the chain that bounds event loops | reader, materializer, host, workflow, engine, runtime |
| `Envelope` | One row change from the database log | Written by `services/cdc-reader`. Read by the materializer and `control/environment/src/capture_gap.rs` |
| `DerivedEvent` | One event published by a workflow terminal | Written by `runtime/src/plugins/wamn_jetstream.rs`. Read by the materializer |
| `subject`, `stream_subjects`, `stream_name`, `subject_token` | NATS subject and stream names | reader, runtime, workflow, `control/provision`, `control/environment`, materializer |
| `msg_id`, `derived_msg_id`, `project_env` | The `Nats-Msg-Id` used to drop duplicates | reader, materializer, workflow, runtime; `project_env` alone: `control/environment` |
| `DeliveryAdvisory`, `DeliveryAdvisoryType`, `delivery_advisory_stream`, `delivery_advisory_subjects`, two retention constants | The broker's own notice that a message ran out of deliveries | `control/environment/src/event_advisories.rs`, `control/provision`, reader (stream name only) |
| `DERIVED_EVENT_FORMAT_VERSION` | `"0.1"` | None outside the crate |

## Findings

| # | Finding | Mark |
| --- | --- | --- |
| 1 | `DerivedEvent` has a hand-written `Serialize` and `Deserialize`, about 95 lines. They rename fields, refuse unknown and repeated fields, and require every field. `Envelope` has a hand-written `Serialize` and a second private struct `EnvelopeWire`, about 60 lines, to leave out absent fields. | fact |
| 1a | A derive with `rename`, `deny_unknown_fields` and `skip_serializing_if` gives the same behavior. | inference. The golden tests in this file would confirm it. Not run. |
| 2 | One field, two spellings, on the same stream. `DerivedEvent` writes `format-version`, `package-id`, `dedup-id` with hyphens and `env_key` with an underscore. `Envelope` writes `package_id`, `commit_ts` with underscores. | fact |
| 3 | The crate header says the format is frozen at 0.1.0 and admits only additive change. The repository rule is greenfield with no clients. | fact for both statements. Inference: the freeze protects no outside reader. |
| 4 | `stream_name` exists twice with the same format string: here and `crates/control/provision/src/name.rs::event_stream_name`. | fact |
| 5 | `crates/control/provision/src/events.rs` builds the subject filter `evt.<org>.<project>.<env>.<entity>.>` by hand. This crate has `subject` and `stream_subjects` but no function for that filter. | fact |
| 6 | `msg_id` is `<project>_<env>:<lsn>`. The comment says the prefix keeps ids apart across environments. `stream_name` gives each org, project and env its own stream. | fact |
| 6a | NATS drops duplicates per stream. So the prefix separates nothing. | inference, from general knowledge of JetStream. Not checked this session. |
| 7 | `subject_token` replaces unsafe characters and, when it changed anything, appends an FNV hash. Its own comment says names from the catalog never need it. Catalog names pass `validate_identifier`, which admits only lowercase letters, digits and single underscores. | fact for the code and the rule. Inference: the replace and hash branch cannot fire for an entity or a package id. |
| 8 | The delivery advisory types describe the broker's message, not a wamn message. Their only readers are the two provisioning crates. | fact |
| 9 | An environment is spelled five ways across the two crates read so far: `<org>--<project>--<env>` (registry key), `evt.<org>.<project>.<env>` (subject), `EVT_<n>_<org>_<n>_<project>_<n>_<env>` (stream), `<project>_<env>` (message id), and a `__` form for CDC objects in `control/provision/src/name.rs`. | fact |

## The open point from crate 01: where the port names live

`MAIN_PORT` and `ERROR_PORT` do not belong here. This crate is about events.

What the code shows (fact): the wiring document that stores a port name is in `wamn-catalog` (`wiring.rs`). `wamn-router` walks the ports and has one caller, `wamn-workflow`. `wamn-workflow` already imports the catalog's wiring types in `wiring_lowering.rs`.

Option: the one definition lives in `wamn-catalog` beside the wiring document. The router reads it from there once it is a module of `wamn-workflow`. The copy in the `jsonata` component is that component's own output port name and stays, because a guest does not depend on the catalog.

## Options

| # | Option | What goes | What is lost |
| --- | --- | --- | --- |
| 1 | Derive the two serializers | About 150 hand-written lines and `EnvelopeWire` | Nothing, if finding 1a holds |
| 2 | One spelling for field names | The mixed hyphen and underscore names | The frozen format. Stored events in a running dev stream stop parsing. |
| 3 | Drop the freeze text and the format-version check until there is an outside reader | `DERIVED_EVENT_FORMAT_VERSION`, the `format_version` field, the check in `from_slice` | A stored event from another version is no longer refused by name. It fails to parse or parses. |
| 4 | One `stream_name`; add nothing | `provision` `event_stream_name` | Nothing |
| 5 | `provision` `events.rs` builds its filter from this crate's functions | The hand-built format string | Nothing |
| 6 | `msg_id` is the LSN alone | `project_env` and the prefix | Nothing, if finding 6a holds |
| 7 | `subject_token` refuses an unsafe name | The replace branch and `raw_hash` | A name with a dot or a space is refused, not rewritten |
| 8 | Move the advisory types to the provisioning code that reads them | About 60 lines leave this crate | Nothing. Touches crates that wait for deploy.md. |
| 9 | Port names in `wamn-catalog` | Two of the three definitions and the agreement test | Nothing |

Finding 9 (five spellings of an environment) has no option here. It follows the hierarchy ruling (Architecture tab); tab 88 finding 9 settles it.

## Rulings (2026-11-10)

Owner ruling: all nine options accepted. A crate has one purpose. The logic and functions it defines are not defined again anywhere else. Single source of truth.

| # | Ruling | Still to verify when the change is made |
| --- | --- | --- |
| 1 | Derive the serializers | The golden tests in this file pass with the derive |
| 2 | One spelling for field names |  |
| 3 | The freeze text and the format-version check go |  |
| 4 | One `stream_name`; `provision` `event_stream_name` goes |  |
| 5 | `provision` `events.rs` builds its filter from this crate |  |
| 6 | The message id is the log position alone | That NATS drops duplicates per stream. If it does not, the prefix stays. |
| 7 | `subject_token` refuses an unsafe name |  |
| 8 | The advisory types move to the provisioning code that reads them |  |
| 9 | The port names live in `wamn-catalog`. This closes the open point of crate 01. |  |

## Not read

- The test half of the file, beyond test names.
- The materializer, the reader and the JetStream plugin, beyond the lines that build or parse these messages.

## Tests deleted, changed or moved (added 2026-11-12)

Checked at `epic/dep7` (b94833fd) by grep. Test names are fact; which ruling each follows is from this tab. All 12 tests are in `apps/platform/events/wire/src/lib.rs`.

Changed:

- `envelope_wire_shape_is_the_v3_draft`, `fully_populated_envelope_freezes_every_field`, `derived_event_is_a_distinct_versioned_wire_without_wal_provenance`: one field-name spelling (ruling 2), no format-version check (ruling 3). They stay as the check that the derive writes the same bytes (ruling 1).
- `msg_id_is_project_env_colon_decimal_lsn`, `derived_msg_id_is_host_scoped_and_keeps_the_author_operand`: the message id is the log position alone (ruling 6).
- `subject_token_neutralizes_nats_specials`, `subject_token_distinguishes_dot_from_underscore`: an unsafe name is refused (ruling 7).

Moved:

- `subject_is_the_v3_grammar`, `stream_binds_only_the_declared_project_environment`, `stream_names_keep_all_declared_coordinates_distinct` to the one name module in `wamn-registry` (tab 88 finding 9), with the `event_stream_name` part of `control/provision/src/name.rs::cdc_names_encode_the_triple_in_both_domains` (ruling 4).

Unchanged: `envelope_decode_requires_both_package_and_entity_identity`, `causation_carries_run_root_depth`. Rulings 5, 8 and 9 have no test here.
