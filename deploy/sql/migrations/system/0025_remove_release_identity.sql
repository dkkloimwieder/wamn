-- Remove the release identity, the deployment attestations and the
-- qualifications of the control store (wamn-t7gif.2,
-- docs/plan/platform-deploy.md §15.1, §17.2 Epic 4). The release is its
-- manifest digest in OCI, and its qualification is an OCI artifact beside it,
-- so publish-release and qualify-release write no row here and nothing reads
-- these tables. Dropping the tables also drops their policies, triggers and
-- the control author's read.

DROP TABLE catalog.deployment_attestations;
DROP TABLE catalog.effective_releases;
DROP TABLE catalog.qualifications;
