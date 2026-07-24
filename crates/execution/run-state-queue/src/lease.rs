//! Lease liveness + renewal — the pure decisions a runner makes while it holds a
//! claim. A lease is a visibility timeout: the claimer must renew (heartbeat) it
//! before it expires, or another replica reclaims the run (crash-safe failover).

use crate::model::Millis;

/// Whether a lease is still held at `now` (its deadline is in the future). An
/// absent deadline is not live (the row is unclaimed / reclaimable).
pub fn lease_live(now: Millis, lease_expires_at: Option<Millis>) -> bool {
    lease_expires_at.is_some_and(|t| t > now)
}

/// The lease deadline for a claim taken at `now` with the given TTL.
pub fn lease_deadline(now: Millis, ttl: Millis) -> Millis {
    now + ttl
}

/// Whether a held lease should be renewed now: it expires within `renew_before`
/// of `now`. Heartbeating on `renew_before` well below the TTL keeps the lease
/// alive across normal work while still releasing it promptly on a crash.
pub fn should_renew(now: Millis, lease_expires_at: Millis, renew_before: Millis) -> bool {
    lease_expires_at - now <= renew_before
}
