// @generated; do not edit.

use wamn_client_tui::screen::Screen;
use wamn_client_tui::submission::SessionBinding;

pub mod screens;

#[path = "../../client/sample.rs"]
pub mod sample;

#[must_use]
pub fn screens(binding: SessionBinding) -> Vec<Screen> {
    vec![
        screens::sample::get(binding.clone()),
        screens::sample::read(binding.clone()),
        screens::sample::record(binding),
    ]
}
