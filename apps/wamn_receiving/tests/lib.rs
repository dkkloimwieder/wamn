//! Application integration tests.

#[cfg(test)]
mod receiving_data_access;
#[cfg(test)]
#[expect(
    dead_code,
    unused_imports,
    reason = "The local cases share fixtures with the cluster target."
)]
mod route_authentication_live;
