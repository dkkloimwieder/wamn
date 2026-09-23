// @generated; do not edit.

use wamn_client_tui::screen::Screen;
use wamn_client_tui::submission::SessionBinding;

pub mod screens;

#[path = "../../client/widget.rs"]
pub mod widget;

#[path = "../../client/widget_maker.rs"]
pub mod widget_maker;

#[path = "../../client/widget_tag.rs"]
pub mod widget_tag;

#[must_use]
pub fn screens(binding: SessionBinding) -> Vec<Screen> {
    vec![
        screens::widget::archive(binding.clone()),
        screens::widget::create(binding.clone()),
        screens::widget::delete(binding.clone()),
        screens::widget::get(binding.clone()),
        screens::widget::list(binding.clone()),
        screens::widget::query(binding.clone()),
        screens::widget::record_batch(binding.clone()),
        screens::widget::update(binding.clone()),
        screens::widget_maker::list(binding.clone()),
        screens::widget_maker::query(binding.clone()),
        screens::widget_tag::update(binding),
    ]
}
