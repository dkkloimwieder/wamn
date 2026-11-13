# 08 `wamn-number-pattern`

Path `crates/platform/number-pattern`. One file, `src/lib.rs`: 477 non-test lines, 75 test lines. No dependencies. Two crates depend on it. The file was read in full. Its callers were opened by search, and the repository was searched for a second parser of the same text. Nothing was built or run.

## One purpose

Parse a document-number pattern such as `PO-{seq:po,width=6}` into parts: literal text, counters with their options, record fields and date formats. (fact)

The name fits. It has no SQL. It defines nothing that is not about patterns.

## Who calls it

| Caller | Use |
| --- | --- |
| `schema/generator/src/generate/numbering.rs`, `generate/data.rs` | Checks a package's patterns at build and reads which fields a pattern needs |
| `platform/runtime/src/plugins/wamn_postgres/numbering.rs`, `numbering/set.rs` | Renders a number from the parsed pattern at run time |

## Single source

The crate says it is the one parser. A search of Rust, KCL, TypeScript and SQL for the token text (`{seq:`, `reset=`, `per=`) found no second parser. (fact for the search)

## Findings

| # | Finding | Mark |
| --- | --- | --- |
| 1 | `identifier` here accepts lowercase letters, digits and `_`, and may start with `_`. The manifest rule for a field name, `catalog/model/src/manifest.rs::validate_identifier`, is narrower: it must start with a letter and has no doubled or trailing `_`. A name that reaches this parser from a manifest has already passed the narrower rule. | fact |
| 2 | The generator parses the same declared pattern in three places: `numbering.rs` and twice in `data.rs`. These are calls to this parser, not copies of it. | fact |

## Options

| # | For | Option | What is lost |
| --- | --- | --- | --- |
| 1 | finding 1 | Leave it. The wider rule only matters for a pattern that names a field no manifest could declare, and the generator already refuses a pattern whose field is not in the model (`numbering.rs`). | Nothing |
| 2 | finding 2 | Decided in the generator's tab |  |

No change is proposed for this crate.

## Not read

- The test half of the file.
- The renderer in the runtime, beyond its imports.

## Tests deleted, changed or moved (added 2026-11-12)

None. No change was ruled for this crate.
