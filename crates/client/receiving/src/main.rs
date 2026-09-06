//! The Receiving operator terminal, as one running binary.
//!
//! Everything that DECIDES anything is in the library beside this file: the
//! state is [`AppState`], every transition is [`reduce`], the envelope is
//! [`record_receipt`], and the screen is [`AppScreen`]. This binary is the
//! shell around those — it reads the deployment from the environment, speaks
//! HTTP, maps key presses onto events, and hands the widget to the shared
//! terminal driver. Nothing here is a decision the reducer has not made, which
//! is why the crate's assertions still live below the terminal.
//!
//! The driver is `wamn-client-terminal`, shared with `wamn dev --tui`. It was
//! the developer client's private module until this binary existed
//! (wamn-10yt.5.9); a second copy of raw mode and a second panic hook is
//! exactly what that move prevents.
//!
//! # Configuration
//!
//! `WAMN_BASE_URL` names the deployment, `WAMN_TOKEN` carries the operator
//! PAT, and `WAMN_HOST` supplies the routing host header when the deployment
//! routes by host. All three are deployment facts, so none of them is
//! compiled in.

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use anyhow::Context as _;
use crossterm::event::{Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt as _;
use serde_json::{Value, json};
use wamn_client::{
    ClientError, CredentialProvider, HttpRequest, HttpResponse, RouteMetadata, StaticPat,
    Transport, WamnClient,
};
use wamn_client_terminal::TerminalSession;
use wamn_receiving_tui::model::{Location, PurchaseOrderRow, ReceiptLine, Screen};
use wamn_receiving_tui::request::{ClientSupplied, record_receipt};
use wamn_receiving_tui::screen::AppScreen;
use wamn_receiving_tui::{AppState, Event, reduce};

/// The four routes this client calls.
///
/// Authored here rather than read from the release: producing route metadata
/// from a release is `wamn-10yt.5.8` and has no in-tree producer yet. The
/// templates are the ones the published Receiving package declares, and they
/// are the same four the crate's workflow proof answers.
const ORDER_QUERY: &str = "/purchase_order/query";
const RECEIPT_SCREEN: &str = "/receiving/load_receipt_screen";
const LOCATION_LIST: &str = "/location/list";
const RECORD_RECEIPT: &str = "/receiving/record_receipt";

/// Which of the receipt screen's two text entries the keyboard is typing into.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Focus {
    /// The highlighted line's quantity.
    #[default]
    Quantity,
    /// The receipt reference.
    Reference,
}

impl Focus {
    const fn toggled(self) -> Self {
        match self {
            Self::Quantity => Self::Reference,
            Self::Reference => Self::Quantity,
        }
    }
}

/// What one key press asks for.
///
/// Separated from the loop so the mapping is a pure function over the screen
/// and the focus, and can be asserted without a terminal.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Action {
    /// Leave the client.
    Quit,
    /// Apply one reducer event.
    Apply(Event),
    /// Move the typing focus.
    ToggleFocus,
    /// Load the highlighted order's receipt screen.
    OpenReceipt,
    /// Send the entered receipt.
    Submit,
    /// The key means nothing here.
    Ignore,
}

/// Map one key press onto what it asks for.
fn action(screen: &Screen, focus: Focus, key: KeyEvent) -> Action {
    // Windows reports press AND release; acting on both doubles every key.
    if key.kind != KeyEventKind::Press {
        return Action::Ignore;
    }
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match (screen, key.code) {
        (_, KeyCode::Char('c' | 'q')) if control => Action::Quit,
        (_, KeyCode::Up) => Action::Apply(Event::MoveUp),
        (_, KeyCode::Down) => Action::Apply(Event::MoveDown),
        (Screen::List, KeyCode::Esc | KeyCode::Char('q')) => Action::Quit,
        (Screen::List, KeyCode::Enter) => Action::OpenReceipt,
        (Screen::Receipt, KeyCode::Esc) => Action::Apply(Event::Back),
        (Screen::Receipt, KeyCode::Char('s')) if control => Action::Submit,
        (Screen::Receipt, KeyCode::Char('l')) if control => Action::Apply(Event::NextLocation),
        (Screen::Receipt, KeyCode::Tab) => Action::ToggleFocus,
        (Screen::Receipt, KeyCode::Backspace) => Action::Apply(Event::BackspaceQuantity),
        // A receipt reference carries digits, so which entry a character
        // reaches is the FOCUS and never the character: guessing from the
        // character would send "GRN-1001" into two different fields.
        (Screen::Receipt, KeyCode::Char(symbol)) if !control => match focus {
            Focus::Quantity => Action::Apply(Event::TypeQuantity(symbol)),
            Focus::Reference => Action::Apply(Event::TypeReference(symbol)),
        },
        _ => Action::Ignore,
    }
}

/// One HTTP exchange over the real network.
#[derive(Debug)]
struct HttpTransport {
    client: reqwest::Client,
}

#[async_trait::async_trait]
impl Transport for HttpTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
        let failed = |error: reqwest::Error| ClientError::Transport {
            detail: error.to_string(),
        };
        let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|error| {
            ClientError::Transport {
                detail: error.to_string(),
            }
        })?;
        let mut builder = self.client.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let response = builder.body(request.body).send().await.map_err(failed)?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(failed)?;
        Ok(HttpResponse { status, body })
    }
}

/// The running client: the deployment, the state, and what is being typed.
struct App {
    client: WamnClient,
    state: AppState,
    focus: Focus,
    requests: u64,
}

impl App {
    /// Apply one event and repaint.
    fn apply(&mut self, event: Event, screen: &mut TerminalSession) -> io::Result<()> {
        self.state = reduce(&self.state, event);
        screen.draw(AppScreen::new(&self.state))
    }

    /// Show `operation` as outstanding before the request is awaited.
    fn begin(&mut self, operation: &str, screen: &mut TerminalSession) -> io::Result<()> {
        self.apply(
            Event::Sent {
                operation: operation.to_owned(),
            },
            screen,
        )
    }

    /// A fresh correlation id, unique within this process.
    fn next_request_id(&mut self) -> String {
        self.requests += 1;
        format!("req-{}", self.requests)
    }
}

/// Invoke one single-item operation and return its value.
async fn call(
    client: &WamnClient,
    template: &str,
    request_id: &str,
    mut item: Value,
) -> Result<Value, ClientError> {
    item["request_id"] = json!(request_id);
    let outcomes = client
        .invoke(&route(template), &BTreeMap::new(), &[item])
        .await?;
    outcomes
        .into_iter()
        .next()
        .expect("one sent item yields one outcome")
        .into_result()
}

fn route(template: &str) -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: template.to_owned(),
    }
}

fn rows(value: &Value) -> &[Value] {
    value["rows"].as_array().map_or(&[], Vec::as_slice)
}

fn text(row: &Value, member: &str) -> String {
    row[member].as_str().unwrap_or_default().to_owned()
}

/// Load the first page of purchase orders.
async fn load_orders(app: &mut App, screen: &mut TerminalSession) -> io::Result<()> {
    app.begin("purchase_order.query", screen)?;
    let request_id = app.next_request_id();
    let event = match call(&app.client, ORDER_QUERY, &request_id, json!({})).await {
        Ok(page) => Event::OrdersLoaded {
            rows: rows(&page)
                .iter()
                .map(|row| PurchaseOrderRow {
                    id: text(row, "id"),
                    number: text(row, "purchase_order_number"),
                    status: text(row, "status"),
                    row_version: row["row_version"].as_i64().unwrap_or_default(),
                })
                .collect(),
            next: page["next"].as_str().map(str::to_owned),
        },
        Err(error) => Event::Failed { error },
    };
    app.apply(event, screen)
}

/// Open receipt entry against the highlighted order and load what it needs.
async fn open_receipt(app: &mut App, screen: &mut TerminalSession) -> io::Result<()> {
    let Some(order) = app.state.highlighted_order().cloned() else {
        return Ok(());
    };
    app.apply(Event::OpenReceipt, screen)?;

    app.begin("receiving.load_receipt_screen", screen)?;
    let request_id = app.next_request_id();
    let item = json!({ "purchase_order_id": order.id });
    let event = match call(&app.client, RECEIPT_SCREEN, &request_id, item).await {
        Ok(value) => Event::ReceiptLoaded {
            lines: rows(&value)
                .iter()
                // The projection answers an order with no lines with a row
                // carrying only the header, which is not a receivable line.
                .filter(|row| !row["line_id"].is_null())
                .map(|row| ReceiptLine {
                    purchase_order_line_id: text(row, "line_id"),
                    line_number: row["line_number"].as_i64().unwrap_or_default(),
                    item_number: text(row, "item_number"),
                    ordered: text(row, "ordered_quantity"),
                    received: text(row, "received_quantity"),
                    remaining: text(row, "remaining_quantity"),
                    entered: String::new(),
                })
                .collect(),
        },
        Err(error) => Event::Failed { error },
    };
    app.apply(event, screen)?;
    if app.state.failure.is_some() {
        return Ok(());
    }

    app.begin("location.list", screen)?;
    let request_id = app.next_request_id();
    let event = match call(&app.client, LOCATION_LIST, &request_id, json!({})).await {
        Ok(value) => Event::LocationsLoaded {
            locations: rows(&value)
                .iter()
                .map(|row| Location {
                    id: text(row, "id"),
                    code: text(row, "location_code"),
                })
                .collect(),
        },
        Err(error) => Event::Failed { error },
    };
    app.apply(event, screen)
}

/// Send the entered receipt.
async fn submit(app: &mut App, screen: &mut TerminalSession) -> io::Result<()> {
    let supplied = ClientSupplied {
        request_id: app.next_request_id(),
        // Stable across every retry of the SAME operator action, because it is
        // DERIVED from what identifies that action rather than minted per
        // attempt: this receipt, against this order, under this reference. A
        // second receipt against the same order is a second reference, which
        // is what a receipt reference is for.
        idempotency_key: format!(
            "{}:{}",
            app.state
                .receiving
                .as_ref()
                .map_or("", |order| order.id.as_str()),
            app.state.receipt_reference
        ),
        occurred_at: chrono::Utc::now().to_rfc3339(),
    };
    let items = match record_receipt(&app.state, &supplied) {
        Ok(items) => items,
        Err(reason) => {
            // The screen already knew, so the operator is told what is missing
            // instead of being sent a request that cannot succeed.
            return app.apply(
                Event::Failed {
                    error: ClientError::Operation {
                        literal: "incomplete_receipt".to_owned(),
                        detail: json!({ "detail": reason }),
                    },
                },
                screen,
            );
        }
    };

    app.begin("receiving.record_receipt", screen)?;
    let sent = app
        .client
        .invoke(&route(RECORD_RECEIPT), &BTreeMap::new(), &items)
        .await
        .and_then(|outcomes| {
            outcomes
                .into_iter()
                .next()
                .expect("one sent item yields one outcome")
                .into_result()
        });
    let event = match sent {
        Ok(value) => Event::ReceiptRecorded {
            receipt_id: text(&value, "receipt_id"),
        },
        Err(error) => Event::Failed { error },
    };
    app.apply(event, screen)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base_url = std::env::var("WAMN_BASE_URL")
        .context("WAMN_BASE_URL must name the deployment this client talks to")?;
    let token =
        std::env::var("WAMN_TOKEN").context("WAMN_TOKEN must carry the operator's access token")?;
    let host = std::env::var("WAMN_HOST").ok();

    let mut app = App {
        client: WamnClient::new(
            base_url,
            host,
            Arc::new(StaticPat::new(token)?) as Arc<dyn CredentialProvider>,
            Arc::new(HttpTransport {
                client: reqwest::Client::new(),
            }) as Arc<dyn Transport>,
        ),
        state: AppState::default(),
        focus: Focus::default(),
        requests: 0,
    };

    let mut events = wamn_client_terminal::events();
    let mut screen = TerminalSession::enter().context("enter the interactive terminal")?;
    load_orders(&mut app, &mut screen).await?;

    while let Some(event) = events.next().await {
        let TerminalEvent::Key(key) = event.context("read a terminal event")? else {
            // A resize, and anything else, is answered by repainting: the
            // screen is a rendering of a state that has not changed.
            screen.draw(AppScreen::new(&app.state))?;
            continue;
        };
        match action(&app.state.screen, app.focus, key) {
            Action::Quit => break,
            Action::Ignore => {}
            Action::ToggleFocus => app.focus = app.focus.toggled(),
            Action::Apply(event) => app.apply(event, &mut screen)?,
            Action::OpenReceipt => open_receipt(&mut app, &mut screen).await?,
            Action::Submit => submit(&mut app, &mut screen).await?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Action, Focus, action};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use wamn_receiving_tui::Event;
    use wamn_receiving_tui::model::Screen;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn control(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    /// The same character reaches a different entry under a different focus —
    /// which is the whole reason the focus exists, since a receipt reference
    /// carries digits too.
    #[test]
    fn a_character_follows_the_focus_and_not_its_own_shape() {
        assert_eq!(
            action(&Screen::Receipt, Focus::Quantity, press(KeyCode::Char('1'))),
            Action::Apply(Event::TypeQuantity('1'))
        );
        assert_eq!(
            action(
                &Screen::Receipt,
                Focus::Reference,
                press(KeyCode::Char('1'))
            ),
            Action::Apply(Event::TypeReference('1'))
        );
    }

    /// Escape leaves the receipt rather than the client: an operator backing
    /// out of an order must not lose the session.
    #[test]
    fn escape_leaves_the_receipt_and_quits_only_from_the_list() {
        assert_eq!(
            action(&Screen::Receipt, Focus::Quantity, press(KeyCode::Esc)),
            Action::Apply(Event::Back)
        );
        assert_eq!(
            action(&Screen::List, Focus::Quantity, press(KeyCode::Esc)),
            Action::Quit
        );
    }

    /// A key RELEASE is not a key press. Terminals that report both would
    /// otherwise type every character twice.
    #[test]
    fn a_release_is_ignored() {
        let mut release = press(KeyCode::Char('7'));
        release.kind = KeyEventKind::Release;
        assert_eq!(
            action(&Screen::Receipt, Focus::Quantity, release),
            Action::Ignore
        );
    }

    /// Sending and cycling the location are control keys, so they stay
    /// reachable while the reference entry is taking plain characters.
    #[test]
    fn sending_and_cycling_stay_reachable_while_typing() {
        assert_eq!(
            action(
                &Screen::Receipt,
                Focus::Reference,
                control(KeyCode::Char('s'))
            ),
            Action::Submit
        );
        assert_eq!(
            action(
                &Screen::Receipt,
                Focus::Reference,
                control(KeyCode::Char('l'))
            ),
            Action::Apply(Event::NextLocation)
        );
    }
}
