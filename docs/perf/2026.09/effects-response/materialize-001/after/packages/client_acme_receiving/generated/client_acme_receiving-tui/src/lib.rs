// @generated; do not edit.

use wamn_client_tui::screen::Screen;
use wamn_client_tui::submission::SessionBinding;

pub mod screens;

#[path = "../../client/purchase_order.rs"]
pub mod purchase_order;

#[path = "../../client/quality.rs"]
pub mod quality;

#[path = "../../client/receiving.rs"]
pub mod receiving;

#[must_use]
pub fn screens(binding: SessionBinding) -> Vec<Screen> {
    vec![
        screens::purchase_order::get(binding.clone()),
        screens::purchase_order::update(binding.clone()),
        screens::quality::approve_inspection(binding.clone()),
        screens::quality::load_purchase_order_detail(binding.clone()),
        screens::receiving::record_receipt(binding),
    ]
}
