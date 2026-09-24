// @generated; do not edit.

use wamn_client_tui::screen::Screen;
use wamn_client_tui::submission::SessionBinding;

pub mod screens;

#[path = "../../client/inventory.rs"]
pub mod inventory;

#[path = "../../client/inventory_movement.rs"]
pub mod inventory_movement;

#[path = "../../client/location.rs"]
pub mod location;

#[path = "../../client/pallet.rs"]
pub mod pallet;

#[path = "../../client/pallet_quantity.rs"]
pub mod pallet_quantity;

#[path = "../../client/product.rs"]
pub mod product;

#[must_use]
pub fn screens(binding: SessionBinding) -> Vec<Screen> {
    vec![
        screens::inventory::adjust(binding.clone()),
        screens::inventory::aggregate(binding.clone()),
        screens::inventory::merge(binding.clone()),
        screens::inventory::r#move(binding.clone()),
        screens::inventory::split(binding.clone()),
        screens::inventory_movement::get(binding.clone()),
        screens::inventory_movement::query(binding.clone()),
        screens::location::create(binding.clone()),
        screens::location::get(binding.clone()),
        screens::location::query(binding.clone()),
        screens::location::update(binding.clone()),
        screens::pallet::create(binding.clone()),
        screens::pallet::get(binding.clone()),
        screens::pallet::query(binding.clone()),
        screens::pallet_quantity::get(binding.clone()),
        screens::pallet_quantity::query(binding.clone()),
        screens::product::create(binding.clone()),
        screens::product::get(binding.clone()),
        screens::product::query(binding.clone()),
        screens::product::update(binding),
    ]
}
