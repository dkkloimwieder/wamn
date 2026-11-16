/// The revision of a key that a host saw before it read the fact of the key.
/// A put with it fails when a writer changed the key during the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket(pub u64);
