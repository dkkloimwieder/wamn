//! Developer-owned screens for receiving.

use wamn_client_tui::{screen, submission};
use ::generated::screens::receiving::{RECORD_RECEIPT_SPEC};

#[must_use]
pub fn record_receipt(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&RECORD_RECEIPT_SPEC, binding)
}
