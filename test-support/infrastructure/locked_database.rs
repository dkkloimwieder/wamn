//! A test database held with the process lock of the per-process PostgreSQL server.

use std::ops::Deref;

/// A database of this test process's PostgreSQL server, held with the process lock.
///
/// The wamn-ctl live tests change roles that every database of the server shares,
/// so each one holds the lock for its whole duration.
#[derive(Debug)]
pub struct LockedDatabase {
    database: wamn_test_postgres::Database,
    _lock: wamn_test_postgres::ProcessLock,
}

/// Take the process lock, then create a database with `floor`.
pub fn database(floor: fn() -> wamn_test_postgres::Database) -> LockedDatabase {
    let lock = wamn_test_postgres::lock();
    LockedDatabase {
        database: floor(),
        _lock: lock,
    }
}

impl Deref for LockedDatabase {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.database.url()
    }
}
