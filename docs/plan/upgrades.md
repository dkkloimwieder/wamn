# Remaining package-upgrade design

[Package upgrade](package-upgrade.md) defines the accepted scope and governance.
[Deployment](../operations/deployment.md#package-upgrades) describes the single-package additive path and retained-schema rollback.
Only Epic 1 is scoped for implementation. The owner opens each later epic after reviewing its predecessor.

## Epic 2: a base package under an overlay

An overlay pins an exact base package version and component digest.
The design must define successor coordinates, revised pins, compatibility proof, and application order for both packages.
Base and overlay migration streams retain their independent definition ownership and immutable prefixes.

Qualification must preserve overlay-owned columns, constraints, and consumed operation contracts on shared relations.
It must prove the complete presented root set and resulting effective privileges against copied installed data.
Existing fresh overlay comparisons do not prove this transition.

## Epic 3: migrations outside the additive policy

The design must cover constraint strengthening, type changes, column removal, and data backfills.
It must define required serving pauses, value-dependent validation, recovery steps, and any online or resumable backfill procedure.
Committed package migrations remain in place after a release rollback.

Migration-specific exceptions require explicit preconditions, validation, and recovery evidence.
One recorded case adds a nullable column that predecessor whole-row SQL cannot read under predecessor grants.
Epic 1 refuses that transition even when candidate grants restore access. Epic 3 owns any exception procedure.

Schema relocation or multiple serving schemas require a persisted deployment fact before they can be qualified.
Their behavior and exception procedures remain outside Epic 1 and have no scoped implementation here.
