//! A node whose only point is a disallowed dependency (`hex`) in its closure.
pub fn touch() -> String {
    hex::encode([0xde, 0xad])
}
