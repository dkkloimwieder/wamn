# Remaining package-upgrade design

[Package upgrade](package-upgrade.md) defines the accepted scope and governance.
[Deployment](../operations/deployment.md#package-upgrades) describes additive package upgrades, coordinated overlay successors, and retained-schema rollback.
Epic 1 is complete. The owner opened Epic 2 after reviewing Epic 1.

## Epic 2: a base package under an overlay

An overlay pins an exact base package version and component digest.
The implementation requires direct successor coordinates, exact revised pins, unchanged consumed contracts, and one transaction for all affected packages.
Base and overlay migration streams retain their independent definition ownership and immutable prefixes.

Owner decisions of 2026-10-02 define the Epic 2 implementation:

- Apply the base first, then every affected overlay successor, in one transaction. A failure rolls back every package and the accepted qualification.
- Admit only additive base migrations. Refuse and name each statement that cannot run in a transaction, including `CREATE INDEX CONCURRENTLY`.
- An overlay successor changes only its package coordinate and its exact base version and component digest. Its migration bytes stay unchanged.
- Every consumed operation contract stays unchanged. A changed or removed contract belongs to Epic 3 and is named in the refusal.
- Qualification binds the complete predecessor and successor root sets and the actual candidate base artifact digest. Ordinary component admission and release qualification remain required.
- PostgreSQL tests start from installed predecessor data and are the Epic 2 exit. Preserve overlay rows, fields, constraints, and definition ownership.
- The later live forward upgrade uses `upgrade-environment` on `wamn-dev` after Epic 2 lands. Record that result on `wamn-orb5`. No live rollback or re-forward is scheduled.

TUI work remains deferred. Epic 3 does not open when Epic 2 closes without a separate owner instruction.

Qualification preserves overlay-owned columns, constraints, and consumed operation contracts on shared relations.
It proves the complete presented root set and resulting effective privileges against copied installed data.
The retained-data PostgreSQL cases pass on 2026-10-02. Their commands and results are recorded on `wamn-orb5`.

## Epic 3: migrations outside the additive policy

The design must cover constraint strengthening, type changes, column removal, and data backfills.
It must define required serving pauses, value-dependent validation, recovery steps, and any online or resumable backfill procedure.
Committed package migrations remain in place after a release rollback.

Migration-specific exceptions require explicit preconditions, validation, and recovery evidence.
One recorded case adds a nullable column that predecessor whole-row SQL cannot read under predecessor grants.
Epic 1 refuses that transition even when candidate grants restore access. Epic 3 owns any exception procedure.

Schema relocation or multiple serving schemas require a persisted deployment fact before they can be qualified.
Their behavior and exception procedures remain outside Epic 1 and have no scoped implementation here.
