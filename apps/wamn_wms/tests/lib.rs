//! Application integration tests.

#[cfg(test)]
#[expect(
    dead_code,
    reason = "The local cases share fixtures with the cluster target."
)]
mod wms_runtime_live;

#[cfg(test)]
#[expect(
    dead_code,
    unused_imports,
    reason = "The local cases share fixtures with the cluster target."
)]
mod delivery;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "The local cases share fixtures with the cluster target."
)]
mod environment;

#[cfg(test)]
mod business_fixture;

#[cfg(test)]
mod local_business;

#[cfg(test)]
mod local_case2;

#[cfg(test)]
mod local_feed;

#[cfg(test)]
mod local_outbox;

#[cfg(test)]
mod local_release;

#[cfg(test)]
mod local_take_offline;

#[cfg(test)]
mod inventory_commands_live;

#[cfg(test)]
mod startup_checks;
#[cfg(test)]
mod startup_support;
