//! Domain navigation and explicit read-to-input mappings for Receiving.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};
use serde_json::{Value, json};
use wamn_client::{ClientError, HttpResponse};
use wamn_client_terminal::operator::{Action, Application, GeneratedApplication, PreparedRequest};
use wamn_client_tui::draft::FieldState;
use wamn_client_tui::screen::{IntentValues, Screen};
use wamn_client_tui::submission::{Attempt, SessionBinding, State};
use wamn_generated_receiving_tui as generated;
use wamn_record_history::{HistoryRow, RowState, state_at};

/// The largest page that one purchase order history request asks for.
const HISTORY_PAGE: i64 = 100;

/// The five generated screens in the Receiving workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Panel {
    Orders,
    Lines,
    Locations,
    Receipt,
    History,
}

/// The rows of every page of one purchase order history read.
#[derive(Debug)]
struct History {
    order: String,
    rows: Vec<Value>,
    complete: bool,
    selected: usize,
}

/// Receiving-specific navigation around generated tables, editors, and submissions.
#[derive(Debug)]
pub struct ReceivingApplication {
    generated: GeneratedApplication,
    queued_read: Option<Panel>,
    order: Option<String>,
    location: Option<usize>,
    projection_ready: bool,
    history: Option<History>,
}

impl ReceivingApplication {
    /// Start with a queued purchase-order read on the supplied activation.
    #[must_use]
    pub fn new(label: &str, binding: SessionBinding) -> Self {
        let mut generated = GeneratedApplication::new(
            label,
            vec![
                generated::screens::purchase_order::query(binding.clone()),
                generated::screens::receiving::load_receipt_screen(binding.clone()),
                generated::screens::location::list(binding.clone()),
                generated::screens::receiving::record_receipt(binding.clone()),
                generated::screens::receiving::load_purchase_order_history(binding),
            ],
        );
        generated.open_screen(Panel::Orders as usize);
        Self {
            generated,
            queued_read: Some(Panel::Orders),
            order: None,
            location: None,
            projection_ready: false,
            history: None,
        }
    }

    /// Inspect a generated screen without bypassing its lifecycle.
    #[must_use]
    pub fn screen(&self, panel: Panel) -> &Screen {
        self.generated.screen(panel as usize)
    }

    /// The current workflow panel, including its shared editor or confirmation.
    #[must_use]
    pub fn panel(&self) -> Panel {
        match self.generated.active_screen() {
            Some(1) => Panel::Lines,
            Some(2) => Panel::Locations,
            Some(3) => Panel::Receipt,
            Some(4) => Panel::History,
            _ => Panel::Orders,
        }
    }

    /// The row highlighted by the generated table.
    #[must_use]
    pub fn selected_row(&self, panel: Panel) -> usize {
        self.generated.selected_row(panel as usize)
    }

    /// The selected location, read from the generated location screen.
    #[must_use]
    pub fn location(&self) -> Option<&Value> {
        self.location
            .and_then(|index| self.screen(Panel::Locations).rows().get(index))
    }

    fn screen_mut(&mut self, panel: Panel) -> &mut Screen {
        self.generated.screen_mut(panel as usize)
    }

    fn show(&mut self, panel: Panel) {
        self.generated.open_screen(panel as usize);
        if panel != Panel::Receipt {
            self.generated.select_results(panel as usize);
        }
    }

    fn open_receipt(&mut self) -> Result<(), String> {
        let orders = self.screen(Panel::Orders);
        if !matches!(orders.submission().state(), State::Succeeded { .. }) {
            return Err("Load the purchase orders before opening a receipt.".into());
        }
        let id = orders
            .rows()
            .get(self.selected_row(Panel::Orders))
            .and_then(|row| row["id"].as_str())
            .ok_or("Select a purchase order.")?
            .to_owned();
        for panel in [Panel::Lines, Panel::Locations, Panel::Receipt] {
            self.screen_mut(panel)
                .new_command()
                .map_err(|error| error.to_string())?;
        }
        self.screen_mut(Panel::Lines)
            .bind("/purchase_order_id", json!(id))
            .map_err(|error| error.to_string())?;
        self.screen_mut(Panel::Receipt)
            .bind("/value/purchase_order_id", json!(id))
            .map_err(|error| error.to_string())?;
        self.order = Some(id);
        self.location = None;
        self.projection_ready = false;
        self.queued_read = Some(Panel::Lines);
        self.show(Panel::Lines);
        Ok(())
    }

    /// The state of the purchase order at the selected history entry.
    ///
    /// # Errors
    /// Returns the reason when the history is not loaded or the fold refuses it.
    pub fn history_state(&self) -> Result<RowState, String> {
        let history = self
            .history
            .as_ref()
            .filter(|history| history.complete)
            .ok_or("The purchase order history is not loaded.")?;
        let rows = history_rows(&history.rows)
            .ok_or("The history read returned a malformed position or image.")?;
        let position = rows.get(history.selected).map_or(0, |row| row.position);
        state_at(&rows, position).map_err(|error| format!("The history cannot fold: {error}."))
    }

    fn open_history(&mut self) -> Result<(), String> {
        let orders = self.screen(Panel::Orders);
        if !matches!(orders.submission().state(), State::Succeeded { .. }) {
            return Err("Load the purchase orders before opening a history.".into());
        }
        let id = orders
            .rows()
            .get(self.selected_row(Panel::Orders))
            .and_then(|row| row["id"].as_str())
            .ok_or("Select a purchase order.")?
            .to_owned();
        self.screen_mut(Panel::History)
            .new_command()
            .map_err(|error| error.to_string())?;
        self.history = Some(History {
            order: id,
            rows: Vec::new(),
            complete: false,
            selected: 0,
        });
        self.generated.open_screen(Panel::History as usize);
        self.request_history_page(0)
    }

    /// Bind the next page of the open history and queue its read.
    fn request_history_page(&mut self, after_position: i64) -> Result<(), String> {
        let order = self
            .history
            .as_ref()
            .map(|history| history.order.clone())
            .ok_or("No purchase order history is open.")?;
        let screen = self.screen_mut(Panel::History);
        for (pointer, value) in [
            ("/id", order),
            ("/after_position", after_position.to_string()),
            ("/limit", HISTORY_PAGE.to_string()),
        ] {
            screen
                .bind(pointer, json!(value))
                .map_err(|error| error.to_string())?;
        }
        self.queued_read = Some(Panel::History);
        Ok(())
    }

    /// Keep the rows of a history page, then read the next page, read again
    /// after a head change, or finish the read.
    fn receive_history_page(&mut self) -> Result<(), String> {
        let page = self.screen(Panel::History).rows().to_vec();
        let history = self
            .history
            .as_mut()
            .ok_or("No purchase order history is open.")?;
        let head = |row: &Value| int64(&row["head_position"]);
        if let (Some(known), Some(next)) = (history.rows.first(), page.first())
            && head(known) != head(next)
        {
            // A write between two pages changes the head, so the rows of the
            // earlier pages no longer fold with the later ones.
            history.rows.clear();
            self.generated.set_message(
                "The purchase order changed while its history loaded. Reading it again.".into(),
            );
            return self.request_history_page(0);
        }
        let received = !page.is_empty();
        history.rows.extend(page);
        let next = history
            .rows
            .last()
            .and_then(|row| Some((int64(&row["position"])?, head(row)?)));
        match next {
            Some((position, head)) if received && position < head => {
                self.request_history_page(position)
            }
            _ => {
                history.complete = true;
                history.selected = history.rows.len().saturating_sub(1);
                Ok(())
            }
        }
    }

    fn history_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => self.return_to_orders(),
            KeyCode::Up => {
                if let Some(history) = &mut self.history {
                    history.selected = history.selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if let Some(history) = &mut self.history {
                    history.selected =
                        (history.selected + 1).min(history.rows.len().saturating_sub(1));
                }
            }
            _ => return None,
        }
        Some(Action::None)
    }

    fn render_history(&self, history: &History, area: Rect, buffer: &mut Buffer) {
        let mut lines = vec![format!("Purchase order {} history", history.order)];
        for (index, row) in history.rows.iter().enumerate() {
            let marker = if index == history.selected { '>' } else { ' ' };
            let field = |name: &str| {
                row[name]
                    .as_str()
                    .map_or_else(|| row[name].to_string(), str::to_owned)
            };
            lines.push(format!(
                "{marker} {} {} {} {} {}",
                field("position"),
                field("kind"),
                field("operation"),
                field("changed_by"),
                field("changed_at")
            ));
        }
        match self.history_state() {
            Ok(RowState::Present(image)) => {
                lines.push("State: present".into());
                lines.extend(
                    image
                        .columns()
                        .map(|(name, value)| format!("  {name} = {value}")),
                );
            }
            Ok(RowState::Absent) => lines.push("State: absent".into()),
            Ok(RowState::Unavailable) => lines.push("State: unavailable".into()),
            Err(error) => lines.push(error),
        }
        Paragraph::new(lines.join("\n")).render(area, buffer);
    }

    fn edit_quantity(&mut self) -> Result<(), String> {
        if !self.projection_ready {
            return Err("Load this purchase order's receipt lines first.".into());
        }
        let id = self
            .screen(Panel::Lines)
            .rows()
            .get(self.selected_row(Panel::Lines))
            .and_then(|row| row["line_id"].as_str())
            .ok_or("This purchase order has no selected line.")?
            .to_owned();
        let rows = self.screen(Panel::Receipt).draft().item()["value"]["line"].as_array();
        let existing = rows.and_then(|rows| {
            rows.iter()
                .position(|row| row["purchase_order_line_id"].as_str() == Some(&id))
        });
        let index = if let Some(index) = existing {
            index
        } else {
            let index = rows.map_or(0, Vec::len);
            let mut row = json!({"purchase_order_line_id": id, "quantity": ""});
            if let Some(location) = self.location() {
                row["location_id"] = location["id"].clone();
            }
            self.screen_mut(Panel::Receipt)
                .insert_row("/value/line", index, row)
                .map_err(|error| error.to_string())?;
            index
        };
        self.generated.edit_field(
            Panel::Receipt as usize,
            &format!("/value/line/{index}/quantity"),
        )
    }

    fn choose_location(&mut self, index: usize) -> Result<(), String> {
        let location = self
            .screen(Panel::Locations)
            .rows()
            .get(index)
            .ok_or("A location is required.")?;
        let id = location["id"].clone();
        let code = location["location_code"].as_str().unwrap_or("").to_owned();
        let count = self.screen(Panel::Receipt).draft().item()["value"]["line"]
            .as_array()
            .map_or(0, Vec::len);
        for row in 0..count {
            self.screen_mut(Panel::Receipt)
                .edit(
                    &format!("/value/line/{row}/location_id"),
                    FieldState::Value(id.clone()),
                )
                .map_err(|error| error.to_string())?;
        }
        self.location = Some(index);
        self.generated.set_message(format!("Receive into {code}."));
        Ok(())
    }

    fn complete_entry(&mut self) -> Result<(), String> {
        if self.order.is_none() {
            return Err("No purchase order is open.".into());
        }
        let receipt = self.screen(Panel::Receipt).draft().item();
        if receipt["value"]["receipt_reference"]
            .as_str()
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err("A receipt reference is required.".into());
        }
        let location = self.location().ok_or("A location is required.")?["id"].clone();
        let lines = receipt["value"]["line"].as_array();
        let mut entered = Vec::new();
        for row in lines.into_iter().flatten() {
            let quantity = row["quantity"].as_str().unwrap_or("").trim();
            if quantity.is_empty() {
                continue;
            }
            if !self.projection_ready
                || !self.screen(Panel::Lines).rows().iter().any(|line| {
                    !line["line_id"].is_null() && line["line_id"] == row["purchase_order_line_id"]
                })
            {
                return Err("The receipt line must belong to the loaded purchase order.".into());
            }
            entered.push(json!({
                "purchase_order_line_id": row["purchase_order_line_id"],
                "location_id": location,
                "quantity": quantity,
            }));
        }
        if entered.is_empty() {
            return Err("Enter a quantity on at least one line.".into());
        }
        self.screen_mut(Panel::Receipt)
            .edit("/value/line", FieldState::Value(json!(entered)))
            .map_err(|error| error.to_string())
    }

    fn return_to_orders(&mut self) {
        self.order = None;
        self.location = None;
        self.projection_ready = false;
        self.history = None;
        self.queued_read = None;
        self.show(Panel::Orders);
    }

    fn domain_key(&mut self, key: KeyEvent) -> Result<Option<Action>, String> {
        let panel = self.panel();
        if panel == Panel::History {
            return Ok(self.history_key(key));
        }
        if key.code == KeyCode::Esc && panel != Panel::Orders {
            // The command owns the dirty draft and any uncertain submission.
            // Its shared confirmation is required even while a read is shown.
            self.show(Panel::Receipt);
            return Ok(Some(self.generated.key(key)));
        }
        if key.code == KeyCode::Char('s')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && panel != Panel::Orders
        {
            self.generated.open_screen(Panel::Receipt as usize);
            return Ok(Some(send(Panel::Receipt)));
        }
        match key.code {
            KeyCode::Enter if panel == Panel::Orders => self.open_receipt()?,
            KeyCode::Char('h') if panel == Panel::Orders => self.open_history()?,
            KeyCode::Enter if panel == Panel::Lines => self.edit_quantity()?,
            KeyCode::Enter if panel == Panel::Locations => {
                self.choose_location(self.selected_row(Panel::Locations))?;
                self.show(Panel::Receipt);
            }
            KeyCode::F(2) if self.order.is_some() => self.show(Panel::Lines),
            KeyCode::F(3) if self.order.is_some() => {
                self.generated
                    .edit_field(Panel::Receipt as usize, "/value/receipt_reference")?;
            }
            KeyCode::F(4) if self.order.is_some() => self.show(Panel::Locations),
            KeyCode::F(9) if self.order.is_some() => self.show(Panel::Receipt),
            KeyCode::Char('l') if self.order.is_some() => {
                let count = self.screen(Panel::Locations).rows().len();
                if count > 0 {
                    self.choose_location((self.location.unwrap_or(0) + 1) % count)?;
                }
            }
            KeyCode::F(6) if self.order.is_some() => {
                return Err("Leave this receipt, then open an order for a new receipt.".into());
            }
            KeyCode::F(5) if self.order.is_some() => {
                return Err(
                    "Leave this receipt, then reopen the order to refresh its lines.".into(),
                );
            }
            _ => return Ok(None),
        }
        Ok(Some(Action::None))
    }
}

fn send(panel: Panel) -> Action {
    Action::Send {
        screen: panel as usize,
        retry: false,
        delete_confirmed: false,
    }
}

impl Application for ReceivingApplication {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let body = Rect {
            height: area.height - 1,
            ..area
        };
        match &self.history {
            Some(history) if history.complete => self.render_history(history, body, buffer),
            _ => self.generated.render(body, buffer),
        }
        let help = if self.history.is_some() {
            "Up/Down entry  Esc back  q quit"
        } else if self.order.is_some() {
            "F2 lines / Enter quantity  F3 reference  F4 locations / l cycle  Ctrl-S send  Esc back"
        } else {
            "Enter receive order  h history  F8 next page  F5 refresh  q quit"
        };
        Paragraph::new(help).render(
            Rect::new(area.x, area.y + area.height - 1, area.width, 1),
            buffer,
        );
    }

    fn key(&mut self, key: KeyEvent) -> Action {
        if key.kind != KeyEventKind::Press {
            return Action::None;
        }
        if self.pending() && key.code != KeyCode::Char('q') {
            self.generated
                .set_message("A request is pending. Wait for its outcome.".into());
            return Action::None;
        }
        let action = if self.generated.browsing() {
            match self.domain_key(key) {
                Ok(Some(action)) => action,
                Ok(None) => self.generated.key(key),
                Err(error) => {
                    self.generated.set_message(error);
                    Action::None
                }
            }
        } else {
            self.generated.key(key)
        };
        if self.generated.active_screen().is_none() && !matches!(action, Action::Exit) {
            self.return_to_orders();
        }
        action
    }

    fn next_action(&mut self) -> Action {
        if self.pending() {
            return Action::None;
        }
        self.queued_read.take().map_or(Action::None, send)
    }

    fn prepare(
        &mut self,
        action: Action,
        intent: Option<&IntentValues>,
    ) -> Result<PreparedRequest, String> {
        if self.pending() {
            return Err("A request is pending. Wait for its outcome.".into());
        }
        if matches!(action, Action::Send { screen, retry: false, .. } if screen == Panel::Receipt as usize)
        {
            self.complete_entry()?;
        }
        self.generated.prepare(action, intent)
    }

    fn resolve(
        &mut self,
        screen: usize,
        attempt: Attempt,
        response: Result<HttpResponse, ClientError>,
    ) {
        let was_pending = self.generated.screen(screen).submission().state() == &State::Pending;
        self.generated.resolve(screen, attempt, response);
        if !was_pending
            || !matches!(
                self.generated.screen(screen).submission().state(),
                State::Succeeded { .. }
            )
        {
            return;
        }
        if screen == Panel::Lines as usize {
            self.projection_ready = self
                .screen(Panel::Lines)
                .rows()
                .iter()
                .all(|row| row["purchase_order_id"].as_str() == self.order.as_deref());
            if self.projection_ready {
                self.queued_read = Some(Panel::Locations);
            } else {
                self.generated.set_message(
                    "The projection returned a different purchase order. Reopen the order.".into(),
                );
            }
        } else if screen == Panel::Locations as usize {
            if !self.screen(Panel::Locations).rows().is_empty()
                && let Err(error) = self.choose_location(0)
            {
                self.generated.set_message(error);
            }
            self.show(Panel::Lines);
        } else if screen == Panel::History as usize {
            if let Err(error) = self.receive_history_page() {
                self.generated.set_message(error);
            }
        } else if screen == Panel::Receipt as usize {
            let receipt = self
                .screen(Panel::Receipt)
                .rows()
                .first()
                .and_then(|row| row["receipt_id"].as_str())
                .unwrap_or("")
                .to_owned();
            self.return_to_orders();
            self.generated
                .set_message(format!("Recorded receipt {receipt}."));
        }
    }

    fn set_message(&mut self, message: String) {
        self.generated.set_message(message);
    }

    fn pending(&self) -> bool {
        self.generated.pending()
    }

    fn unresolved(&self) -> bool {
        self.generated.unresolved()
    }
}

/// An int64 result value, spelled as a JSON string or a JSON number.
fn int64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

/// The fold rows of a history read, or `None` for a malformed row.
fn history_rows(rows: &[Value]) -> Option<Vec<HistoryRow<'_>>> {
    rows.iter()
        .map(|row| {
            Some(HistoryRow {
                position: int64(&row["position"])?,
                kind: row["kind"].as_str()?,
                before: row["before"].as_str()?,
                current: row["current"].as_str()?,
                head_position: int64(&row["head_position"])?,
            })
        })
        .collect()
}
