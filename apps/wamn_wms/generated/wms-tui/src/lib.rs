// @generated; do not edit.

use wamn_client_tui::screen::Screen;
use wamn_client_tui::submission::SessionBinding;

pub mod screens;

#[path = "../../client/inventory.rs"]
pub mod inventory;

#[path = "../../client/inventory_transaction.rs"]
pub mod inventory_transaction;

#[path = "../../client/location.rs"]
pub mod location;

#[path = "../../client/packaging.rs"]
pub mod packaging;

#[path = "../../client/product.rs"]
pub mod product;

#[must_use]
pub fn screens(binding: SessionBinding) -> Vec<Screen> {
    vec![
        screens::inventory::adjust(binding.clone()),
        screens::inventory::aggregate(binding.clone()),
        screens::inventory::get(binding.clone()),
        screens::inventory::merge(binding.clone()),
        screens::inventory::r#move(binding.clone()),
        screens::inventory::query(binding.clone()),
        screens::inventory::split(binding.clone()),
        screens::inventory_transaction::get(binding.clone()),
        screens::inventory_transaction::query(binding.clone()),
        screens::location::create(binding.clone()),
        screens::location::get(binding.clone()),
        screens::location::query(binding.clone()),
        screens::location::update(binding.clone()),
        screens::packaging::close(binding.clone()),
        screens::packaging::create(binding.clone()),
        screens::packaging::get(binding.clone()),
        screens::packaging::query(binding.clone()),
        screens::product::create(binding.clone()),
        screens::product::get(binding.clone()),
        screens::product::query(binding.clone()),
        screens::product::update(binding),
    ]
}
