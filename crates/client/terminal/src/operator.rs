//! The shared operator loop for generated and composed screens.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io;
use std::pin::Pin;
use std::process::{ExitCode, Termination};
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt as _;
use futures_util::stream::FuturesUnordered;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Paragraph, Widget, Wrap};
use serde_json::{Value, json};
use wamn_client::descriptor::{FieldDescriptor, FieldSchema};
use wamn_client::request::BuiltRequest;
use wamn_client::{
    ClientError, HttpRequest, HttpResponse, RouteMetadata, StaticPat, Transport, WamnClient,
};
use wamn_client_tui::draft::{FieldState, InputKind, input_kind};
use wamn_client_tui::screen::{ExitState, IntentValues, Screen, ScreenSpec};
use wamn_client_tui::submission::{Attempt, SessionBinding, State, recovery_message};

use crate::TerminalSession;

/// Exit status acknowledging SIGTERM after the terminal has been restored.
///
/// The supervisor distinguishes this conventional 128 + 15 status from operator quit.
pub const SUPERVISOR_STOP_EXIT_CODE: u8 = 143;

/// Why the shared terminal loop ended after restoring the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The operator quit, sent Ctrl-C or SIGINT, or closed the input stream.
    Operator,
    /// The supervisor requested termination with SIGTERM.
    SupervisorStop,
}

impl Termination for ExitReason {
    fn report(self) -> ExitCode {
        match self {
            Self::Operator => ExitCode::SUCCESS,
            Self::SupervisorStop => ExitCode::from(SUPERVISOR_STOP_EXIT_CODE),
        }
    }
}

#[derive(Debug)]
struct HttpTransport(reqwest::Client);

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
        let mut builder = self.0.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let response = builder.body(request.body).send().await.map_err(failed)?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(failed)?;
        Ok(HttpResponse { status, body })
    }
}

fn required_env(name: &str) -> Result<String, io::Error> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("{name} is required")))
}

/// Run one operator terminal bound to the activation supplied at launch.
///
/// `WAMN_BASE_URL`, `WAMN_TOKEN` and `WAMN_TARGET_INSTANCE` are required;
/// `WAMN_HOST` optionally supplies the routing host. Credentials are obtained
/// through the shared provider for every HTTP attempt.
///
/// # Errors
/// Returns configuration, signal-registration or terminal I/O failures.
pub async fn run(
    label: &str,
    screens: impl Fn(SessionBinding) -> Vec<Screen>,
) -> Result<ExitReason, Box<dyn Error>> {
    run_application(label, |label, binding| {
        GeneratedApplication::new(label, screens(binding))
    })
    .await
}

/// Run a composed application using the shared terminal and request loop.
///
/// The factory receives the label and active target binding. The driver owns
/// credentials, HTTP attempts, intent values, signals, and terminal restoration.
///
/// # Errors
/// Returns configuration, signal-registration or terminal I/O failures.
pub async fn run_application<A: Application>(
    label: &str,
    factory: impl FnOnce(&str, SessionBinding) -> A,
) -> Result<ExitReason, Box<dyn Error>> {
    let binding = SessionBinding {
        url: required_env("WAMN_BASE_URL")?,
        host: std::env::var("WAMN_HOST")
            .ok()
            .filter(|host| !host.is_empty()),
        target_instance: required_env("WAMN_TARGET_INSTANCE")?,
    };
    let credentials = Arc::new(StaticPat::new(required_env("WAMN_TOKEN")?)?);
    let transport = Arc::new(HttpTransport(reqwest::Client::new()));
    let client = Arc::new(WamnClient::new(
        binding.url.clone(),
        binding.host.clone(),
        credentials,
        transport,
    ));
    run_application_with_client(label, binding, client, factory).await
}

/// Run a composed application with its prepared client and active target binding.
///
/// The caller completes login before this function enters the terminal. The driver
/// retains the same request, signal, and terminal restoration rules as `run_application`.
///
/// # Errors
/// Returns signal-registration or terminal I/O failures.
pub async fn run_application_with_client<A: Application>(
    label: &str,
    binding: SessionBinding,
    client: Arc<WamnClient>,
    factory: impl FnOnce(&str, SessionBinding) -> A,
) -> Result<ExitReason, Box<dyn Error>> {
    let mut app = factory(label, binding);
    let mut events = crate::events();
    let shutdown = shutdown_signal()?;
    tokio::pin!(shutdown);
    let mut pending: FuturesUnordered<Pending> = FuturesUnordered::new();
    let mut terminal = TerminalSession::enter()?;
    let result = async {
        let mut exit_reason = ExitReason::Operator;
        loop {
            terminal.draw(ApplicationWidget(&app))?;
            let transport_pending = !pending.is_empty();
            let queued = queued_application_action(&mut app, transport_pending);
            let action = if matches!(queued, Action::None) {
                tokio::select! {
                    signal = &mut shutdown => { exit_reason = signal?; Action::Exit },
                    Some((index, attempt, response)) = pending.next(), if transport_pending => {
                        app.resolve(index, attempt, response);
                        Action::None
                    },
                    event = events.next() => match event {
                        Some(Ok(Event::Key(key))) => application_key(&mut app, key, transport_pending),
                        Some(Ok(_)) => Action::None,
                        Some(Err(error)) => return Err(error),
                        None => Action::Exit,
                    },
                }
            } else {
                queued
            };
            if matches!(action, Action::Exit) {
                break;
            }
            if matches!(action, Action::Send { .. }) {
                match prepare_application(&mut app, action) {
                    Ok(request) => {
                        let client = client.clone();
                        pending.push(Box::pin(
                            async move { submit_request(&client, &request).await },
                        ));
                    }
                    Err(error) => app.set_message(error),
                }
            }
        }
        Ok::<ExitReason, io::Error>(exit_reason)
    }
    .await;
    let unresolved = app.unresolved() || !pending.is_empty();
    drop(terminal);
    drop(pending);
    if unresolved {
        eprintln!(
            "Client exited. The server request was not cancelled; its outcome is unknown and it may still complete."
        );
    }
    Ok(result?)
}

/// Application decisions around the shared request and terminal machinery.
///
/// Implementations compose generated Screens and delegate each result to the
/// Screen that owns its Attempt. They never send HTTP themselves.
pub trait Application {
    fn render(&self, area: Rect, buffer: &mut Buffer);
    fn key(&mut self, key: KeyEvent) -> Action;
    /// Supply an initial or follow-up action when no application or transport request is pending.
    fn next_action(&mut self) -> Action {
        Action::None
    }
    /// Prepare a validated request. A new intent receives values; a retry receives None.
    ///
    /// # Errors
    /// Returns a user-facing validation or lifecycle refusal.
    fn prepare(
        &mut self,
        action: Action,
        intent: Option<&IntentValues>,
    ) -> Result<PreparedRequest, String>;
    fn resolve(
        &mut self,
        screen: usize,
        attempt: Attempt,
        response: Result<HttpResponse, ClientError>,
    );
    fn set_message(&mut self, message: String);
    /// True while any owned Screen has a pending attempt.
    fn pending(&self) -> bool;
    /// True while server work is pending or its completion remains uncertain.
    fn unresolved(&self) -> bool;
}

struct ApplicationWidget<'a, A>(&'a A);
impl<A: Application> Widget for ApplicationWidget<'_, A> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        self.0.render(area, buffer);
    }
}

fn queued_application_action(app: &mut impl Application, transport_pending: bool) -> Action {
    if transport_pending || app.pending() {
        Action::None
    } else {
        app.next_action()
    }
}

fn application_key(app: &mut impl Application, key: KeyEvent, transport_pending: bool) -> Action {
    // Raw mode delivers Ctrl-C as a key. Only the loop may force this exit.
    if key.kind == KeyEventKind::Press
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.code == KeyCode::Char('c')
    {
        return Action::Exit;
    }
    let action = app.key(key);
    if matches!(action, Action::Exit) && (app.pending() || transport_pending) {
        app.set_message(
            "A request is pending. Wait, or Ctrl-C to exit without cancelling server work.".into(),
        );
        Action::None
    } else {
        action
    }
}

fn prepare_application(
    app: &mut impl Application,
    action: Action,
) -> Result<PreparedRequest, String> {
    let intent = matches!(action, Action::Send { retry: false, .. }).then(|| IntentValues {
        request_id: uuid::Uuid::new_v4().to_string(),
        idempotency_key: uuid::Uuid::new_v4().to_string(),
        occurred_at: chrono::Utc::now().to_rfc3339(),
    });
    app.prepare(action, intent.as_ref())
}

async fn submit_request(
    client: &WamnClient,
    request: &PreparedRequest,
) -> (usize, Attempt, Result<HttpResponse, ClientError>) {
    let response = if request.fresh_only {
        client
            .submit_fresh(&request.route, &request.parameters, &request.body)
            .await
    } else {
        client
            .submit(&request.route, &request.parameters, &request.body)
            .await
    };
    (request.screen, request.attempt, response)
}

type Pending =
    Pin<Box<dyn Future<Output = (usize, Attempt, Result<HttpResponse, ClientError>)> + Send>>;

fn shutdown_signal() -> io::Result<impl Future<Output = io::Result<ExitReason>>> {
    #[cfg(unix)]
    {
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        Ok(async move {
            tokio::select! {
                _ = interrupt.recv() => Ok(ExitReason::Operator),
                _ = terminate.recv() => Ok(ExitReason::SupervisorStop),
            }
        })
    }
    #[cfg(not(unix))]
    Ok(async { tokio::signal::ctrl_c().await.map(|()| ExitReason::Operator) })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Pane {
    #[default]
    Inputs,
    Results,
}

#[derive(Debug, Default)]
struct View {
    input: usize,
    row: usize,
    column: usize,
    scroll: usize,
    pane: Pane,
    parameters: BTreeMap<String, String>,
    captured_parameters: Option<BTreeMap<String, String>>,
    parameters_dirty: bool,
}

#[derive(Debug)]
enum Target {
    Field(String),
    Parameter(String),
}

#[derive(Debug, Clone, Copy)]
enum Confirmation {
    Leave,
    LeaveUncertain,
    Quit,
    Delete,
    NewCommand,
}

#[derive(Debug, Default)]
enum Mode {
    #[default]
    Browse,
    Text {
        target: Target,
        value: String,
    },
    Choice {
        pointer: String,
        values: &'static [&'static str],
        selected: usize,
    },
    Confirm(Confirmation),
    Links {
        candidates: Vec<usize>,
        selected: usize,
        row: usize,
    },
}

/// A keyboard decision; ordinary Exit is refused by the loop while pending.
#[derive(Debug, Clone, Copy)]
pub enum Action {
    None,
    Exit,
    Send {
        screen: usize,
        retry: bool,
        delete_confirmed: bool,
    },
}

/// One validated, captured attempt ready for the shared HTTP loop.
#[derive(Debug)]
pub struct PreparedRequest {
    /// Application-owned request destination, returned unchanged to resolve.
    pub screen: usize,
    pub attempt: Attempt,
    pub route: RouteMetadata,
    /// Request a fresh credential when the operation declares that requirement.
    pub fresh_only: bool,
    pub parameters: BTreeMap<String, String>,
    pub body: BuiltRequest,
}

/// The standard generated-screen menus, editors, and result views.
#[derive(Debug)]
pub struct GeneratedApplication {
    label: String,
    screens: Vec<Screen>,
    views: Vec<View>,
    models: Vec<&'static str>,
    model: Option<usize>,
    selected: usize,
    active: Option<usize>,
    mode: Mode,
    message: String,
}

impl GeneratedApplication {
    /// Create the default application, also usable as a composition's fallback.
    #[must_use]
    pub fn new(label: &str, screens: Vec<Screen>) -> Self {
        let views = screens
            .iter()
            .map(|screen| View {
                parameters: screen
                    .spec()
                    .route
                    .map(|route| route_parameters(&route()))
                    .unwrap_or_default(),
                ..View::default()
            })
            .collect();
        let models = screens
            .iter()
            .map(|screen| screen.spec().model)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Self {
            label: label.into(),
            screens,
            models,
            model: None,
            views,
            selected: 0,
            active: None,
            mode: Mode::Browse,
            message: String::new(),
        }
    }

    /// Read a screen from the fixed constructor list.
    #[must_use]
    pub fn screen(&self, index: usize) -> &Screen {
        &self.screens[index]
    }

    /// Edit a screen through its validated bindings and lifecycle methods.
    pub fn screen_mut(&mut self, index: usize) -> &mut Screen {
        &mut self.screens[index]
    }

    /// Return the currently open screen's constructor index.
    #[must_use]
    pub const fn active_screen(&self) -> Option<usize> {
        self.active
    }

    /// Return the selected result row for a screen.
    #[must_use]
    pub fn selected_row(&self, index: usize) -> usize {
        self.views[index].row
    }

    /// True when a field editor, choice, or confirmation does not own the keyboard.
    #[must_use]
    pub fn browsing(&self) -> bool {
        matches!(self.mode, Mode::Browse)
    }

    /// Open a screen with its result pane selected.
    pub fn select_results(&mut self, index: usize) {
        self.open_screen(index);
        self.views[index].pane = Pane::Results;
    }

    /// Open the shared editor for a declared, unreserved input field.
    ///
    /// # Errors
    /// Refuses an unknown, reserved, or unsupported field.
    pub fn edit_field(&mut self, index: usize, pointer: &str) -> Result<(), String> {
        let rows = editor_rows(
            self.screens[index].spec().input,
            self.screens[index].draft().item(),
        );
        let (position, row) = rows
            .iter()
            .enumerate()
            .find(|(_, row)| row.pointer == pointer)
            .ok_or("The requested field is not declared.")?;
        if reserved(self.screens[index].spec(), row.schema.field.path) {
            return Err(
                "This value is supplied by the platform or a declared record binding.".into(),
            );
        }
        if row.kind == InputKind::Unsupported {
            return Err("This field requires a composed editor.".into());
        }
        self.open_screen(index);
        self.views[index].pane = Pane::Inputs;
        self.views[index].input = self.views[index].parameters.len() + position;
        self.open_editor(index, row);
        Ok(())
    }

    fn operations(&self) -> Vec<usize> {
        self.screens
            .iter()
            .enumerate()
            .filter_map(|(index, screen)| {
                self.model
                    .is_some_and(|model| self.models[model] == screen.spec().model)
                    .then_some(index)
            })
            .collect()
    }

    /// Open a screen by its index in the fixed constructor list.
    pub fn open_screen(&mut self, index: usize) {
        self.model = self
            .models
            .iter()
            .position(|model| *model == self.screens[index].spec().model);
        self.selected = self
            .operations()
            .iter()
            .position(|candidate| *candidate == index)
            .unwrap_or(0);
        self.active = Some(index);
    }

    fn leave_screen(&mut self, index: usize) {
        self.open_screen(index);
        self.active = None;
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.kind != KeyEventKind::Press {
            return Action::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Exit;
        }
        let mode = std::mem::take(&mut self.mode);
        match mode {
            Mode::Text { target, mut value } => {
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Enter => self.commit(target, value),
                    KeyCode::Backspace => {
                        value.pop();
                        self.mode = Mode::Text { target, value };
                    }
                    KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if self.accepts_character(&target, &value, character) {
                            value.push(character);
                        }
                        self.mode = Mode::Text { target, value };
                    }
                    _ => self.mode = Mode::Text { target, value },
                }
                return Action::None;
            }
            Mode::Choice {
                pointer,
                values,
                mut selected,
            } => {
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Enter => {
                        self.commit(Target::Field(pointer), values[selected].to_owned());
                    }
                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                        self.mode = Mode::Choice {
                            pointer,
                            values,
                            selected,
                        };
                    }
                    KeyCode::Down => {
                        selected = (selected + 1).min(values.len() - 1);
                        self.mode = Mode::Choice {
                            pointer,
                            values,
                            selected,
                        };
                    }
                    _ => {
                        self.mode = Mode::Choice {
                            pointer,
                            values,
                            selected,
                        }
                    }
                }
                return Action::None;
            }
            Mode::Confirm(confirmation) => {
                if key.code == KeyCode::Char('y') {
                    return self.confirm(confirmation);
                }
                if !matches!(key.code, KeyCode::Esc | KeyCode::Char('n')) {
                    self.mode = Mode::Confirm(confirmation);
                }
                return Action::None;
            }
            Mode::Links {
                candidates,
                mut selected,
                row,
            } => {
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Enter => self.open_link(candidates[selected], row),
                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                        self.mode = Mode::Links {
                            candidates,
                            selected,
                            row,
                        };
                    }
                    KeyCode::Down => {
                        selected = (selected + 1).min(candidates.len() - 1);
                        self.mode = Mode::Links {
                            candidates,
                            selected,
                            row,
                        };
                    }
                    _ => {
                        self.mode = Mode::Links {
                            candidates,
                            selected,
                            row,
                        }
                    }
                }
                return Action::None;
            }
            Mode::Browse => {}
        }
        if key.code == KeyCode::Char('q') {
            return self.quit();
        }
        let Some(index) = self.active else {
            let operations = self.operations();
            let count = if self.model.is_some() {
                operations.len()
            } else {
                self.models.len()
            };
            match key.code {
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down => self.selected = (self.selected + 1).min(count.saturating_sub(1)),
                KeyCode::Enter if count > 0 => {
                    if self.model.is_some() {
                        self.open_screen(operations[self.selected]);
                    } else {
                        self.model = Some(self.selected);
                        self.selected = 0;
                    }
                    self.message.clear();
                }
                KeyCode::Esc => {
                    if let Some(model) = self.model.take() {
                        self.selected = model;
                    } else {
                        return self.quit();
                    }
                }
                _ => {}
            }
            return Action::None;
        };
        match key.code {
            KeyCode::Esc => {
                if matches!(
                    self.screens[index].submission().state(),
                    State::Uncertain { .. }
                ) {
                    self.mode = Mode::Confirm(Confirmation::LeaveUncertain);
                    return Action::None;
                }
                match self.screens[index].exit_state() {
                    ExitState::Pending => self.message = "Request pending; wait for its outcome. Ctrl-C exits without cancelling server work.".into(),
                    ExitState::ConfirmDiscard => self.mode = Mode::Confirm(Confirmation::Leave),
                    ExitState::Ready if self.views[index].parameters_dirty => self.mode = Mode::Confirm(Confirmation::Leave),
                    ExitState::Ready => self.leave_screen(index),
                }
            }
            KeyCode::Tab => {
                self.views[index].pane = if self.views[index].pane == Pane::Inputs {
                    Pane::Results
                } else {
                    Pane::Inputs
                }
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.screens[index].spec().kind == "delete" {
                    self.mode = Mode::Confirm(Confirmation::Delete);
                } else {
                    return Action::Send {
                        screen: index,
                        retry: false,
                        delete_confirmed: false,
                    };
                }
            }
            KeyCode::F(5) => return self.refresh(index),
            KeyCode::F(6) => {
                if matches!(
                    self.screens[index].submission().state(),
                    State::Uncertain { .. }
                ) {
                    self.mode = Mode::Confirm(Confirmation::NewCommand);
                } else {
                    self.reset(index);
                }
            }
            KeyCode::F(7) => {
                return Action::Send {
                    screen: index,
                    retry: true,
                    delete_confirmed: false,
                };
            }
            KeyCode::F(8) => match self.screens[index].next_page() {
                Ok(()) => {
                    return Action::Send {
                        screen: index,
                        retry: false,
                        delete_confirmed: false,
                    };
                }
                Err(error) => self.message = error.to_string(),
            },
            _ if self.views[index].pane == Pane::Inputs => self.input_key(index, key),
            _ => self.result_key(index, key),
        }
        Action::None
    }

    fn quit(&mut self) -> Action {
        if self
            .screens
            .iter()
            .any(|screen| screen.exit_state() == ExitState::Pending)
        {
            self.message =
                "A request is pending. Wait, or Ctrl-C to exit without cancelling server work."
                    .into();
            Action::None
        } else if self
            .screens
            .iter()
            .any(|screen| screen.exit_state() == ExitState::ConfirmDiscard)
            || self.views.iter().any(|view| view.parameters_dirty)
        {
            self.mode = Mode::Confirm(Confirmation::Quit);
            Action::None
        } else {
            Action::Exit
        }
    }

    fn confirm(&mut self, confirmation: Confirmation) -> Action {
        match confirmation {
            Confirmation::Quit => Action::Exit,
            Confirmation::Leave | Confirmation::LeaveUncertain => {
                if let Some(index) = self.active {
                    self.reset(index);
                    self.leave_screen(index);
                }
                Action::None
            }
            Confirmation::Delete => Action::Send {
                screen: self.active.expect("confirmation has a screen"),
                retry: false,
                delete_confirmed: true,
            },
            Confirmation::NewCommand => {
                if let Some(index) = self.active {
                    self.reset(index);
                }
                Action::None
            }
        }
    }

    fn reset(&mut self, index: usize) {
        match self.screens[index].new_command() {
            Ok(()) => {
                self.views[index] = View {
                    parameters: self.screens[index]
                        .spec()
                        .route
                        .map(|route| route_parameters(&route()))
                        .unwrap_or_default(),
                    ..View::default()
                };
                self.message = "New command. Previous server work was not cancelled.".into();
            }
            Err(error) => self.message = error.to_string(),
        }
    }

    fn input_key(&mut self, index: usize, key: KeyEvent) {
        let rows = editor_rows(
            self.screens[index].spec().input,
            self.screens[index].draft().item(),
        );
        let parameters = self.views[index]
            .parameters
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let count = rows.len() + parameters.len();
        self.views[index].input = self.views[index].input.min(count.saturating_sub(1));
        match key.code {
            KeyCode::Up => {
                self.views[index].input = self.views[index].input.saturating_sub(1);
                return;
            }
            KeyCode::Down => {
                self.views[index].input =
                    (self.views[index].input + 1).min(count.saturating_sub(1));
                return;
            }
            _ => {}
        }
        let focus = self.views[index].input;
        if focus < parameters.len() {
            if key.code == KeyCode::Enter {
                if matches!(self.screens[index].submission().state(), State::Editable) {
                    let name = parameters[focus].clone();
                    self.mode = Mode::Text {
                        value: self.views[index].parameters[&name].clone(),
                        target: Target::Parameter(name),
                    };
                } else {
                    self.message = "Start a new command before changing route parameters.".into();
                }
            }
            return;
        }
        let Some(row) = rows.get(focus - parameters.len()) else {
            return;
        };
        if reserved(self.screens[index].spec(), row.schema.field.path) {
            self.message =
                "This value is supplied by the platform or a declared record binding.".into();
            return;
        }
        match key.code {
            KeyCode::Enter => self.open_editor(index, row),
            KeyCode::Char('a') if !row.schema.required && !row.object_row => {
                let result = self.screens[index].edit(&row.pointer, FieldState::Absent);
                self.report(result);
            }
            KeyCode::Char('n') if row.schema.field.nullable && !row.object_row => {
                let result = self.screens[index].edit(&row.pointer, FieldState::Null);
                self.report(result);
            }
            KeyCode::Char('+') if row.kind == InputKind::Repeated => self.append_row(index, row),
            KeyCode::Char('-') | KeyCode::Delete => {
                if let Some((array, number)) = &row.parent_row {
                    let result = self.screens[index].remove_row(array, *number);
                    self.report(result);
                }
            }
            _ => {}
        }
    }

    fn open_editor(&mut self, index: usize, row: &EditorRow) {
        if row.object_row {
            if !self.screens[index]
                .draft()
                .item()
                .pointer(&row.pointer)
                .is_some_and(Value::is_object)
            {
                let result = self.screens[index].edit(&row.pointer, FieldState::Value(json!({})));
                self.report(result);
            }
            return;
        }
        match row.kind {
            InputKind::Text => {
                self.mode = Mode::Text {
                    target: Target::Field(row.pointer.clone()),
                    value: self.screens[index]
                        .draft()
                        .item()
                        .pointer(&row.pointer)
                        .filter(|value| !value.is_null())
                        .map(display_value)
                        .unwrap_or_default(),
                }
            }
            InputKind::Choice => {
                let current = self.screens[index]
                    .draft()
                    .item()
                    .pointer(&row.pointer)
                    .and_then(Value::as_str);
                let values = row.schema.field.values;
                self.mode = Mode::Choice {
                    pointer: row.pointer.clone(),
                    selected: values
                        .iter()
                        .position(|value| Some(*value) == current)
                        .unwrap_or(0),
                    values,
                };
            }
            InputKind::Object => {
                if !self.screens[index]
                    .draft()
                    .item()
                    .pointer(&row.pointer)
                    .is_some_and(Value::is_object)
                {
                    let result =
                        self.screens[index].edit(&row.pointer, FieldState::Value(json!({})));
                    self.report(result);
                }
            }
            InputKind::Repeated => self.append_row(index, row),
            InputKind::Unsupported => {
                self.message = "Unsupported input: this operation needs a composed editor.".into();
            }
        }
    }

    fn append_row(&mut self, index: usize, row: &EditorRow) {
        if self.screens[index].draft().item().pointer(&row.pointer) == Some(&Value::Null)
            && let Err(error) = self.screens[index].edit(&row.pointer, FieldState::Value(json!([])))
        {
            self.message = error.to_string();
            return;
        }
        let count = self.screens[index]
            .draft()
            .item()
            .pointer(&row.pointer)
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let value = array_element(row.schema).map_or_else(
            || json!({}),
            |child| match input_kind(child) {
                InputKind::Repeated => json!([]),
                InputKind::Object => json!({}),
                InputKind::Choice => json!(child.field.values[0]),
                InputKind::Text if !child.field.nullable => json!(""),
                _ => Value::Null,
            },
        );
        let result = self.screens[index].insert_row(&row.pointer, count, value);
        self.report(result);
    }

    fn accepts_character(&self, target: &Target, value: &str, character: char) -> bool {
        let Target::Field(pointer) = target else {
            return true;
        };
        let index = self.active.expect("editor has an active screen");
        let numeric = editor_rows(
            self.screens[index].spec().input,
            self.screens[index].draft().item(),
        )
        .iter()
        .any(|row| row.pointer == *pointer && row.schema.field.type_name == "numeric");
        !numeric
            || character.is_ascii_digit()
            || (character == '.' && !value.contains('.'))
            || (character == '-' && value.is_empty())
    }

    fn commit(&mut self, target: Target, value: String) {
        let index = self.active.expect("editor has an active screen");
        match target {
            Target::Field(pointer) => {
                let result = self.screens[index].edit(&pointer, FieldState::Value(json!(value)));
                self.report(result);
            }
            Target::Parameter(name) => {
                self.views[index].parameters.insert(name, value);
                self.views[index].parameters_dirty = true;
                self.message.clear();
            }
        }
    }

    fn report(&mut self, result: Result<(), impl std::fmt::Display>) {
        self.message = result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
    }

    fn result_key(&mut self, index: usize, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.views[index].row = self.views[index].row.saturating_sub(1),
            KeyCode::Down => {
                self.views[index].row = (self.views[index].row + 1)
                    .min(self.screens[index].rows().len().saturating_sub(1));
            }
            KeyCode::Left => self.views[index].column = self.views[index].column.saturating_sub(1),
            KeyCode::Right => {
                self.views[index].column = (self.views[index].column + 1).min(
                    result_fields(self.screens[index].spec().response.fields)
                        .len()
                        .saturating_sub(1),
                );
            }
            KeyCode::PageUp => {
                self.views[index].scroll = self.views[index].scroll.saturating_sub(5);
            }
            KeyCode::PageDown => {
                self.views[index].scroll = self.views[index].scroll.saturating_add(5);
            }
            KeyCode::Enter => {
                let candidates = link_targets(&self.screens, index);
                if !candidates.is_empty() && !self.screens[index].rows().is_empty() {
                    self.mode = Mode::Links {
                        candidates,
                        selected: 0,
                        row: self.views[index].row,
                    };
                } else {
                    self.message = "No declared record link is available; choose another operation from the menu.".into();
                }
            }
            _ => {}
        }
    }

    fn open_link(&mut self, target: usize, row: usize) {
        let source_index = self.active.expect("link has a source");
        let (source, target_screen) = if source_index < target {
            let (before, after) = self.screens.split_at_mut(target);
            (&before[source_index], &mut after[0])
        } else {
            let (before, after) = self.screens.split_at_mut(source_index);
            (&after[0], &mut before[target])
        };
        let result = if target_screen
            .spec()
            .revision
            .is_some_and(|revision| revision.read_operation == source.spec().operation)
        {
            target_screen.bind_from_read(source)
        } else {
            target_screen.populate_read_from_record(source, row)
        };
        match result {
            Ok(()) => {
                self.open_screen(target);
                self.views[target].pane = Pane::Inputs;
                self.message = "Declared fields were bound. Complete any remaining required inputs before submitting.".into();
            }
            Err(error) => self.message = error.to_string(),
        }
    }

    fn refresh(&mut self, index: usize) -> Action {
        let target = if matches!(
            self.screens[index].spec().kind,
            "get" | "query" | "projection"
        ) {
            Some(index)
        } else {
            self.screens[index].spec().revision.and_then(|revision| {
                self.screens
                    .iter()
                    .position(|screen| screen.spec().operation == revision.read_operation)
            })
        };
        let Some(target) = target else {
            self.message = format!(
                "{} Select a read operation to refresh data.",
                recovery_message(&self.screens[index].spec().response)
            );
            return Action::None;
        };
        if target != index {
            let (command, read) = if index < target {
                let (before, after) = self.screens.split_at_mut(target);
                (&before[index], &mut after[0])
            } else {
                let (before, after) = self.screens.split_at_mut(index);
                (&after[0], &mut before[target])
            };
            if let Err(error) = command.prepare_record_refresh(read) {
                self.message = error.to_string();
                return Action::None;
            }
        }
        match self.screens[target].refresh() {
            Ok(()) => {
                self.open_screen(target);
                Action::Send {
                    screen: target,
                    retry: false,
                    delete_confirmed: false,
                }
            }
            Err(error) => {
                self.message = error.to_string();
                Action::None
            }
        }
    }

    fn complete(
        &mut self,
        index: usize,
        attempt: Attempt,
        response: Result<HttpResponse, ClientError>,
    ) {
        if !self.screens[index].resolve(attempt, response) {
            return;
        }
        let view = &mut self.views[index];
        match self.screens[index].submission().state() {
            State::Succeeded { .. } | State::PartiallyCompleted { .. } => {
                view.pane = Pane::Results;
                view.row = 0;
                view.column = 0;
                view.scroll = 0;
            }
            State::Refused(error) => {
                view.pane = Pane::Inputs;
                if let Some(field) = error.pointer("/detail/field").and_then(Value::as_str)
                    && let Some(position) = editor_rows(
                        self.screens[index].spec().input,
                        self.screens[index].draft().item(),
                    )
                    .iter()
                    .position(|row| row.schema.field.path == field || row.pointer == field)
                {
                    view.input = view.parameters.len() + position;
                }
            }
            _ => {}
        }
    }

    fn prepare_request(
        &mut self,
        action: Action,
        intent: Option<&IntentValues>,
    ) -> Result<PreparedRequest, String> {
        let Action::Send {
            screen: index,
            retry,
            delete_confirmed,
        } = action
        else {
            return Err("No submission was requested.".into());
        };
        let route = self
            .screens
            .get(index)
            .ok_or("The requested screen does not exist.")?
            .spec()
            .route
            .ok_or("This operation is not exposed over HTTP.")?();
        let parameters = if retry {
            self.views[index]
                .captured_parameters
                .clone()
                .ok_or("No captured route parameters exist.")?
        } else {
            self.views[index].parameters.clone()
        };
        if parameters.values().any(String::is_empty) {
            return Err("Complete every route parameter before submitting.".into());
        }
        route.path(&parameters).map_err(|error| error.to_string())?;
        let attempt = if retry {
            self.screens[index].retry()
        } else {
            self.screens[index].begin(
                intent.ok_or("A new submission requires driver-supplied intent values.")?,
                delete_confirmed,
            )
        }
        .map_err(|error| error.to_string())?;
        let body = self.screens[index]
            .submission()
            .captured()
            .expect("successful begin captures the request")
            .clone();
        if !retry {
            self.views[index].captured_parameters = Some(parameters.clone());
        }
        self.views[index].parameters_dirty = false;
        self.message.clear();
        Ok(PreparedRequest {
            screen: index,
            attempt,
            route,
            fresh_only: self.screens[index].spec().fresh_only,
            parameters,
            body,
        })
    }
}

impl Application for GeneratedApplication {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        AppWidget(self).render(area, buffer);
    }
    fn key(&mut self, key: KeyEvent) -> Action {
        self.handle_key(key)
    }
    fn prepare(
        &mut self,
        action: Action,
        intent: Option<&IntentValues>,
    ) -> Result<PreparedRequest, String> {
        self.prepare_request(action, intent)
    }
    fn resolve(
        &mut self,
        screen: usize,
        attempt: Attempt,
        response: Result<HttpResponse, ClientError>,
    ) {
        self.complete(screen, attempt, response);
    }
    fn set_message(&mut self, message: String) {
        self.message = message;
    }
    fn pending(&self) -> bool {
        self.screens
            .iter()
            .any(|screen| matches!(screen.submission().state(), State::Pending))
    }
    fn unresolved(&self) -> bool {
        self.screens.iter().any(|screen| {
            matches!(
                screen.submission().state(),
                State::Pending | State::Uncertain { .. }
            )
        })
    }
}

fn route_parameters(route: &RouteMetadata) -> BTreeMap<String, String> {
    route
        .template
        .split('/')
        .filter_map(|segment| {
            segment
                .strip_prefix("{*")
                .or_else(|| segment.strip_prefix('{'))
                .and_then(|name| name.strip_suffix('}'))
                .map(|name| (name.to_owned(), String::new()))
        })
        .collect()
}

fn link_targets(screens: &[Screen], source: usize) -> Vec<usize> {
    let from = screens[source].spec();
    screens
        .iter()
        .enumerate()
        .filter_map(|(index, screen)| {
            let to = screen.spec();
            if index == source || to.route.is_none() {
                return None;
            }
            let revision = to
                .revision
                .is_some_and(|revision| revision.read_operation == from.operation);
            let record = to.kind == "get"
                && from.record.zip(to.record).is_some_and(|(from, to)| {
                    from.relation == to.relation
                        && from.key_field == to.key_field
                        && to.key_input.is_some()
                });
            (revision || record).then_some(index)
        })
        .collect()
}

fn reserved(spec: &ScreenSpec, path: &str) -> bool {
    spec.supplied.iter().any(|field| field.path == path)
        || spec.revision_inputs.contains(&path)
        || (spec.response.result_class == Some("page") && path == "cursor")
}

#[derive(Debug)]
struct EditorRow {
    pointer: String,
    schema: &'static FieldSchema,
    kind: InputKind,
    object_row: bool,
    parent_row: Option<(String, usize)>,
}

fn editor_rows(fields: &'static [FieldSchema], item: &Value) -> Vec<EditorRow> {
    let mut rows = Vec::new();
    append_fields(&mut rows, fields, item, "", None);
    rows
}

fn append_fields(
    rows: &mut Vec<EditorRow>,
    fields: &'static [FieldSchema],
    item: &Value,
    prefix: &str,
    parent_row: Option<&(String, usize)>,
) {
    for schema in fields {
        let name = schema
            .field
            .leaf()
            .trim_end_matches("[]")
            .replace('~', "~0")
            .replace('/', "~1");
        let pointer = format!("{prefix}/{name}");
        append_field(rows, schema, item, &pointer, parent_row);
    }
}

fn array_element(schema: &FieldSchema) -> Option<&FieldSchema> {
    match schema.children {
        [child]
            if child.field.path == schema.field.path
                || child.field.path.strip_suffix("[]") == Some(schema.field.path) =>
        {
            Some(child)
        }
        _ => None,
    }
}

fn append_field(
    rows: &mut Vec<EditorRow>,
    schema: &'static FieldSchema,
    item: &Value,
    pointer: &str,
    parent_row: Option<&(String, usize)>,
) {
    rows.push(EditorRow {
        pointer: pointer.to_owned(),
        schema,
        kind: input_kind(schema),
        object_row: false,
        parent_row: parent_row.cloned(),
    });
    if schema.field.type_name == "object" {
        append_fields(rows, schema.children, item, pointer, parent_row);
    } else if schema.field.type_name == "array" {
        for index in 0..item
            .pointer(pointer)
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
        {
            let path = format!("{pointer}/{index}");
            let parent = Some((pointer.to_owned(), index));
            if let Some(child) = array_element(schema) {
                append_field(rows, child, item, &path, parent.as_ref());
            } else {
                rows.push(EditorRow {
                    pointer: path.clone(),
                    schema,
                    kind: InputKind::Object,
                    object_row: true,
                    parent_row: parent.clone(),
                });
                append_fields(rows, schema.children, item, &path, parent.as_ref());
            }
        }
    }
}

fn display_value(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn result_fields(fields: &[FieldSchema]) -> Vec<FieldDescriptor> {
    fields
        .iter()
        .flat_map(|field| {
            if field.children.is_empty() {
                vec![field.field]
            } else {
                result_fields(field.children)
            }
        })
        .collect()
}

fn result_value(row: &Value, path: &str) -> Option<Value> {
    let (name, rest) = path
        .split_once('.')
        .map_or((path, None), |(name, rest)| (name, Some(rest)));
    let value = row.get(name.trim_end_matches("[]"))?;
    match rest {
        None => Some(value.clone()),
        Some(rest) if name.ends_with("[]") => Some(Value::Array(
            value
                .as_array()?
                .iter()
                .map(|row| result_value(row, rest).unwrap_or(Value::Null))
                .collect(),
        )),
        Some(rest) => result_value(value, rest),
    }
}

fn error_text(error: &Value) -> String {
    let literal = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    format!("{literal}: {}", error.get("detail").unwrap_or(error))
}

fn state_text(screen: &Screen) -> String {
    match screen.submission().state() {
        State::Editable => screen.availability().to_string(),
        State::Pending => "Pending. Exiting the client does not cancel server work.".into(),
        State::Succeeded { opaque, .. } => if *opaque {
            "Succeeded; result is opaque JSON."
        } else {
            "Succeeded. This intent is spent; use F6 for a new command."
        }
        .into(),
        State::Refused(error) => format!("Refused: {}", error_text(error)),
        State::PartiallyCompleted { failed_outcome, .. } => format!(
            "Partially completed; committed work remains. {}",
            error_text(failed_outcome)
        ),
        State::Uncertain {
            reason,
            retry_refusal,
        } => format!(
            "Outcome unknown: {reason}. {}{}{}",
            if reason.ends_with("; the server reported fresh-credential-required") {
                "This operation requires a PAT. The request was not retried. "
            } else {
                ""
            },
            recovery_message(&screen.spec().response),
            retry_refusal
                .as_ref()
                .map(|error| format!(" Captured retry was refused: {}", error_text(error)))
                .unwrap_or_default()
        ),
    }
}

struct AppWidget<'a>(&'a GeneratedApplication);

impl Widget for AppWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let app = self.0;
        let regions = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(5),
            Constraint::Length(3),
        ])
        .split(area);
        let title = app.active.map_or_else(
            || {
                app.model.map_or_else(
                    || format!("{} — models", app.label),
                    |model| format!("{} — {} operations", app.label, app.models[model]),
                )
            },
            |index| {
                format!(
                    "{} — {} / {} ({})",
                    app.label,
                    app.screens[index].spec().model,
                    app.screens[index].spec().name,
                    app.screens[index].spec().kind
                )
            },
        );
        Paragraph::new(title).render(regions[0], buffer);
        if let Some(index) = app.active {
            render_screen(app, index, regions[1], buffer);
        } else {
            let labels = if app.model.is_some() {
                app.operations()
                    .into_iter()
                    .map(|index| {
                        let screen = &app.screens[index];
                        format!(
                            "{} [{}] {}",
                            screen.spec().name,
                            screen.spec().kind,
                            screen.availability()
                        )
                    })
                    .collect::<Vec<_>>()
            } else {
                app.models.iter().map(|model| (*model).to_owned()).collect()
            };
            let lines = labels
                .into_iter()
                .enumerate()
                .map(|(index, label)| {
                    format!("{} {label}", if index == app.selected { ">" } else { " " })
                })
                .collect::<Vec<_>>();
            Paragraph::new(lines.join("\n"))
                .block(Block::bordered().title(if app.model.is_some() {
                    "Select an operation"
                } else {
                    "Select a model"
                }))
                .scroll((scroll_to(app.selected, regions[1].height), 0))
                .render(regions[1], buffer);
        }
        let status = app
            .active
            .map(|index| state_text(&app.screens[index]))
            .unwrap_or_default();
        Paragraph::new(format!("{status}\n{}", app.message))
            .wrap(Wrap { trim: false })
            .render(regions[2], buffer);
        Paragraph::new("Enter edit/open | Tab inputs/results | a absent | n null | + row | - remove\nCtrl-S submit | F5 refresh | F6 new | F7 retry captured | F8 next page\nEsc back | q quit (waits for pending) | Ctrl-C exit; does not cancel server work").render(regions[3], buffer);
        let modal = match &app.mode {
            Mode::Browse => return,
            Mode::Text { target, value } => format!(
                "{}\n{value}_\nEnter saves; Esc cancels",
                match target {
                    Target::Field(path) => path,
                    Target::Parameter(name) => name,
                }
            ),
            Mode::Choice {
                values, selected, ..
            } => values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    format!("{} {value}", if index == *selected { ">" } else { " " })
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Mode::Confirm(confirmation) => format!(
                "{} (y/n)",
                match confirmation {
                    Confirmation::Leave => "Discard draft changes and return to operations?",
                    Confirmation::LeaveUncertain =>
                        "The original request may still complete. Abandon its captured intent and return to operations?",
                    Confirmation::Quit => "Discard draft changes and quit?",
                    Confirmation::Delete =>
                        "Submit this delete with its bound record and revision?",
                    Confirmation::NewCommand =>
                        "The original request may still complete. Abandon its captured intent and start a new command?",
                }
            ),
            Mode::Links {
                candidates,
                selected,
                ..
            } => candidates
                .iter()
                .enumerate()
                .map(|(position, index)| {
                    format!(
                        "{} {} / {}",
                        if position == *selected { ">" } else { " " },
                        app.screens[*index].spec().model,
                        app.screens[*index].spec().name
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        ratatui::widgets::Clear.render(regions[1], buffer);
        Paragraph::new(modal)
            .block(Block::bordered().title("Operator action"))
            .wrap(Wrap { trim: false })
            .render(regions[1], buffer);
    }
}

fn scroll_to(selected: usize, height: u16) -> u16 {
    u16::try_from(selected.saturating_sub(usize::from(height.saturating_sub(3))))
        .unwrap_or(u16::MAX)
}

fn render_screen(app: &GeneratedApplication, index: usize, area: Rect, buffer: &mut Buffer) {
    let screen = &app.screens[index];
    let view = &app.views[index];
    let areas =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
    let rows = editor_rows(screen.spec().input, screen.draft().item());
    let invalid = match screen.submission().state() {
        State::Refused(error) => error.pointer("/detail/field").and_then(Value::as_str),
        _ => None,
    };
    let mut lines = view
        .parameters
        .iter()
        .map(|(name, value)| format!("route {{{name}}} *: {value}"))
        .collect::<Vec<_>>();
    for row in rows {
        let state = if screen.spec().response.result_class == Some("page")
            && row.schema.field.path == "cursor"
        {
            "managed by paging".into()
        } else {
            match screen.draft().item().pointer(&row.pointer) {
                None => "Absent".into(),
                Some(Value::Null) => "Null".into(),
                Some(value) => display_value(value),
            }
        };
        let attributes = if row.object_row {
            "row".into()
        } else {
            format!(
                "{} {}{}{}",
                row.schema.field.type_name,
                if row.schema.required {
                    "required"
                } else {
                    "optional"
                },
                if row.schema.field.nullable {
                    ", nullable"
                } else {
                    ""
                },
                if reserved(screen.spec(), row.schema.field.path) {
                    ", bound"
                } else {
                    ""
                }
            )
        };
        let refusal = if invalid
            .is_some_and(|field| field == row.schema.field.path || field == row.pointer)
        {
            "! "
        } else {
            ""
        };
        let unsupported = if row.kind == InputKind::Unsupported {
            " [unsupported]"
        } else {
            ""
        };
        let bounds = if row.kind == InputKind::Repeated {
            format!(
                " [rows {}..{}]",
                row.schema.minimum.unwrap_or(0),
                row.schema
                    .maximum
                    .map_or_else(|| "unbounded".into(), |maximum| maximum.to_string())
            )
        } else {
            String::new()
        };
        lines.push(format!(
            "{refusal}{} ({attributes}){unsupported}{bounds}: {state}",
            row.pointer
        ));
    }
    let lines = lines
        .into_iter()
        .enumerate()
        .map(|(position, line)| {
            format!(
                "{} {line}",
                if view.pane == Pane::Inputs && position == view.input {
                    ">"
                } else {
                    " "
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Paragraph::new(lines)
        .block(Block::bordered().title(if view.pane == Pane::Inputs {
            "Inputs [active]"
        } else {
            "Inputs"
        }))
        .scroll((scroll_to(view.input, areas[0].height), 0))
        .render(areas[0], buffer);
    let result_area = areas[1];
    let title = format!(
        "Results{} | {} rows{} | Enter links | arrows columns | PgUp/PgDn scroll",
        if view.pane == Pane::Results {
            " [active]"
        } else {
            ""
        },
        screen.rows().len(),
        if screen.cursor().is_some() {
            ", next available"
        } else {
            ""
        }
    );
    let block = Block::bordered().title(title);
    let inner = block.inner(result_area);
    block.render(result_area, buffer);
    let raw = match screen.submission().state() {
        State::Succeeded {
            value,
            opaque: true,
        } => Some(serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())),
        State::PartiallyCompleted {
            committed_result,
            failed_outcome,
        } => Some(format!(
            "Committed result: {committed_result}\nFailed outcome: {}",
            error_text(failed_outcome)
        )),
        _ => None,
    };
    if let Some(raw) = raw {
        Paragraph::new(raw)
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(view.scroll).unwrap_or(u16::MAX), 0))
            .render(inner, buffer);
    } else {
        let fields = result_fields(screen.spec().response.fields);
        if screen.spec().response.result_class == Some("one") {
            let text = screen.rows().first().map_or_else(
                || "No result yet.".into(),
                |row| {
                    fields
                        .iter()
                        .map(|field| {
                            format!(
                                "{}: {}",
                                field.path,
                                result_value(row, field.path)
                                    .as_ref()
                                    .map_or_else(|| "Absent".into(), display_value)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                },
            );
            Paragraph::new(text)
                .scroll((u16::try_from(view.scroll).unwrap_or(u16::MAX), 0))
                .render(inner, buffer);
        } else {
            let column = view.column.min(fields.len());
            let fields = &fields[column..];
            let start = view
                .row
                .saturating_sub(usize::from(inner.height.saturating_sub(2)));
            let rows = screen.rows()[start.min(screen.rows().len())..]
                .iter()
                .map(|row| {
                    fields
                        .iter()
                        .map(|field| {
                            result_value(row, field.path)
                                .as_ref()
                                .map_or_else(|| "Absent".into(), display_value)
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            wamn_client_tui::Table::new(fields, &rows)
                .select(Some(view.row - start))
                .render(inner, buffer);
        }
    }
}

#[cfg(test)]
#[path = "operator_tests.rs"]
mod tests;
