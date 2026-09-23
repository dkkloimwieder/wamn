// @generated; do not edit.

use wamn_client_tui::screen::Screen;
use wamn_client_tui::submission::SessionBinding;

pub mod screens;

#[path = "../../client/widget.rs"]
pub mod widget;

#[must_use]
pub fn screens(binding: SessionBinding) -> Vec<Screen> {
    vec![screens::widget::get(binding)]
}
