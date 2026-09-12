// @generated; do not edit.

use wamn_client_tui::screen::Screen;
use wamn_client_tui::submission::SessionBinding;

pub mod screens;

#[path = "../../client/inventory.rs"]
pub mod inventory;

#[path = "../../client/pallet.rs"]
pub mod pallet;

#[must_use]
pub fn screens(binding: SessionBinding) -> Vec<Screen> {
    vec![
        screens::inventory::adjust(binding.clone()),
        screens::inventory::aggregate(binding.clone()),
        screens::inventory::merge(binding.clone()),
        screens::inventory::r#move(binding.clone()),
        screens::inventory::split(binding.clone()),
        screens::pallet::get(binding.clone()),
        screens::pallet::query(binding),
    ]
}
