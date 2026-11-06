//! Application integration tests.

#[cfg(test)]
mod wms_runtime_live;

#[cfg(test)]
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
