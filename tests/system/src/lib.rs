//! Black-box proofs that drive deployed or public repository surfaces.

#[path = "../../../test-support/fixtures/apifixture.rs"]
mod apifixture;
#[path = "../../../test-support/fixtures/f1fixture.rs"]
mod f1fixture;
#[cfg(test)]
#[path = "../../conformance/src/schema_drift.rs"]
mod schema_drift;

mod apiproof;
mod credproof;
mod f1proof;
mod ladderproof;
mod traceproof;
