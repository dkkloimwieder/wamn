//! The control library that `services/ctl`, the dev loop, and test support call.
//!
//! It holds the admission, release, provisioning, reconcile, and package
//! operations. It does database, filesystem, and network work. It does no CLI
//! presentation and owns no process.
