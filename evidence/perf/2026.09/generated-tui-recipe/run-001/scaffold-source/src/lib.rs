//! Developer-owned composition over regenerated screen contracts.

use wamn_client_tui::screen::Screen;
use wamn_client_tui::submission::SessionBinding;

pub mod screens;
pub use ::generated::location;
pub use ::generated::purchase_order;
pub use ::generated::receipt;
pub use ::generated::receiving;

#[must_use]
pub fn screens(binding: SessionBinding) -> Vec<Screen> {
    vec![
        ::generated::screens::location::list(binding.clone()),
        ::generated::screens::purchase_order::get(binding.clone()),
        ::generated::screens::purchase_order::query(binding.clone()),
        ::generated::screens::purchase_order::update(binding.clone()),
        ::generated::screens::receipt::get(binding.clone()),
        ::generated::screens::receipt::query(binding.clone()),
        ::generated::screens::receiving::load_receipt_screen(binding.clone()),
        screens::receiving::record_receipt(binding),
    ]
}
