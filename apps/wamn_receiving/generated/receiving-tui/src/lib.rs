// @generated; do not edit.

use wamn_client_tui::screen::Screen;
use wamn_client_tui::submission::SessionBinding;

pub mod screens;

#[path = "../../client/location.rs"]
pub mod location;

#[path = "../../client/purchase_order.rs"]
pub mod purchase_order;

#[path = "../../client/receipt.rs"]
pub mod receipt;

#[path = "../../client/receiving.rs"]
pub mod receiving;

#[path = "../../client/supplier.rs"]
pub mod supplier;

#[must_use]
pub fn screens(binding: SessionBinding) -> Vec<Screen> {
    vec![
        screens::location::list(binding.clone()),
        screens::purchase_order::get(binding.clone()),
        screens::purchase_order::query(binding.clone()),
        screens::purchase_order::update(binding.clone()),
        screens::receipt::get(binding.clone()),
        screens::receipt::query(binding.clone()),
        screens::receiving::load_purchase_order_history(binding.clone()),
        screens::receiving::load_receipt_screen(binding.clone()),
        screens::receiving::record_receipt(binding.clone()),
        screens::supplier::create(binding.clone()),
        screens::supplier::query(binding),
    ]
}
