//! Shared WMS rows for local business and deployed delivery tests.

#[derive(Debug, Clone)]
pub struct RuntimePhase {
    /// The fixture packaging the contention moves.
    pub packaging_id: String,
    /// The fixture location it moves to.
    pub to_location_id: String,
}
