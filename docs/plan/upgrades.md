# Schema changes after installation

This is a deferred design for changing a schema while retaining installed application data.
Current provisioning supports fresh installations only, including the control database.
The [current data rules](../architecture/data-access.md) remain authoritative until this design receives its own implementation scope.

## Candidate and predecessor

A candidate must name the exact currently installed package version as its predecessor.
Its cumulative migration paths and checksums must contain the predecessor as a byte-identical prefix.
Preparation applies only the new suffix to a representative copy of the prior database.
It must inspect both the migration history and resulting effective schema.

Base and client packages retain independent migration streams and definition ownership.
A base change cannot alter, remove, conflict with, or invalidate a client-owned definition.
This includes a client column stored on a base-owned table.
Dependent constraints and consumed operation contracts must remain valid too.

The proposed first changes are additive and compatible with the active predecessor.
A version label alone does not establish compatibility.
Exact schema and consumed-contract comparisons determine whether unchanged client artifacts remain usable.
A changed client implementation requires a new client package version.
Existing package coordinates and artifact digests never acquire different contents.

## Preparation and refusal

The candidate's migration must run under the same restricted authority expected during installation.
Its fixture must include data relevant to the change.
Null values, duplicate candidates, foreign-key references, constraint boundaries, and backfilled rows are examples of relevant predecessor states.

Preparation must preserve client-owned definitions and resolve declared dependencies to exact candidate implementations.
It must inspect the complete application SQL corpus against the resulting schema.
Relevant base and client tests must exercise the changed behavior.
Successful preparation leaves the candidate inactive until an explicit deployment selection.

A conflicting definition or incompatible consumed contract must refuse the candidate.
The currently selected application remains active when its database remains compatible and intact.
A preparation result does not grant permission to mutate a production database.
Existing [fresh overlay comparisons](../../apps/client_acme_receiving/overlay-scenario.md) do not establish this upgrade path.

## Installation and activation

The proposed first implementation assumes one deployment writer.
That writer must compare the current predecessor again before applying the tested suffix.
It must establish the resulting schema identity, permissions, and exact bindings before activating the candidate.
Migration or validation failure must prevent activation.

The design still needs an explicit failure rule when database changes complete but later activation fails.
A previous code release is a usable rollback target only while the new schema satisfies its required contracts.
Selecting old code does not reverse committed data changes.
The current fresh-install implementation supplies no existing-data rollback promise.

Drain-required changes, destructive migrations, online backfills, and resumable backfills remain outside the proposed initial scope.
Concurrent deployment coordination and a general deployment recovery process need separate requirements.
No unsupported existing-data upgrade starts from this document alone.
