-- Remove the release head and the selections of the control store
-- (docs/plan/platform-deploy.md §13, §15.1). `env apply` writes the release
-- chart, and Helm keeps its history with the actor and the reason, so neither
-- deploy-release nor select-release remains to write these rows. Dropping the
-- tables also drops their policies, triggers and the control author's read.

DROP TABLE catalog.release_selections;
DROP TABLE catalog.effective_release_heads;
