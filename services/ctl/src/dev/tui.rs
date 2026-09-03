//! The developer terminal client for one local development-loop session.
//!
//! Two layers meet here and stay separable. [`DevTui`], [`DevTuiState`] and
//! [`apply_key`] are pure: they turn one immutable [`DevSnapshot`] and one key
//! press into a Ratatui buffer and new navigation state, with no terminal and
//! no development-loop effects. The private [`terminal`] module underneath is
//! the repository's only terminal driver — raw mode, the alternate screen, an
//! event stream and restoration on panic — and names nothing about this loop,
//! so the second caller can move it out rather than rewrite it
//! (wamn-10yt.5.9).
//!
//! [`run`] joins the two: it holds the activated environment open until the
//! developer quits, and quitting leaves through the session's own native
//! cleanup path rather than a terminal-specific teardown.

use std::io;
use std::pin::pin;

use anyhow::Context as _;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt as _;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

use super::command::{DevCommandArgs, DevSession, DevSessionControl, print_receipt};
use super::read::{DevGateVerdict, DevSnapshot, DevStageState};

/// Lines a page-up or page-down key moves the viewport.
const PAGE_SCROLL_LINES: usize = 10;

/// One of the developer client's three fixed views.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DevTuiPage {
    /// Ordered development stages and their Gate verdicts.
    #[default]
    Pipeline,
    /// Exact release carrier and manifest-derived serving facts.
    Release,
    /// Recent Tempo traces and router-tap records.
    Observations,
}

impl DevTuiPage {
    /// Stable label rendered in the page selector.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pipeline => "pipeline",
            Self::Release => "release",
            Self::Observations => "observations",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Pipeline => Self::Release,
            Self::Release => Self::Observations,
            Self::Observations => Self::Pipeline,
        }
    }

    const fn previous(self) -> Self {
        match self {
            Self::Pipeline => Self::Observations,
            Self::Release => Self::Pipeline,
            Self::Observations => Self::Release,
        }
    }
}

/// Keyboard-controlled view state, independent of terminal lifecycle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DevTuiState {
    page: DevTuiPage,
    vertical_offset: usize,
}

impl DevTuiState {
    /// Start at the top of `page`.
    pub const fn new(page: DevTuiPage) -> Self {
        Self {
            page,
            vertical_offset: 0,
        }
    }

    /// Currently selected page.
    pub const fn page(self) -> DevTuiPage {
        self.page
    }

    /// Requested vertical line offset.
    pub const fn vertical_offset(self) -> usize {
        self.vertical_offset
    }

    /// Select a page and return its viewport to the first row.
    pub fn select_page(&mut self, page: DevTuiPage) {
        self.page = page;
        self.vertical_offset = 0;
    }

    /// Select the next page, wrapping after observations.
    pub fn next_page(&mut self) {
        self.select_page(self.page.next());
    }

    /// Select the previous page, wrapping before the pipeline.
    pub fn previous_page(&mut self) {
        self.select_page(self.page.previous());
    }

    /// Move down by `lines`; rendering clamps past the final viewport.
    pub fn scroll_down(&mut self, lines: usize) {
        self.vertical_offset = self.vertical_offset.saturating_add(lines);
    }

    /// Move up by `lines`, stopping at the first row.
    pub fn scroll_up(&mut self, lines: usize) {
        self.vertical_offset = self.vertical_offset.saturating_sub(lines);
    }
}

/// A backend-free view over one immutable development snapshot.
#[derive(Debug)]
pub struct DevTui<'a> {
    snapshot: &'a DevSnapshot,
    state: &'a DevTuiState,
}

impl<'a> DevTui<'a> {
    /// Render `snapshot` according to `state`.
    pub const fn new(snapshot: &'a DevSnapshot, state: &'a DevTuiState) -> Self {
        Self { snapshot, state }
    }
}

impl Widget for DevTui<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let header = format!(
            "wamn dev | {} {} {} | revision={} | row={}",
            page_label(self.state.page, DevTuiPage::Pipeline),
            page_label(self.state.page, DevTuiPage::Release),
            page_label(self.state.page, DevTuiPage::Observations),
            self.snapshot.revision(),
            self.state.vertical_offset,
        );
        write_line(buffer, area, area.y, &header);
        if area.height == 1 {
            return;
        }

        write_line(
            buffer,
            area,
            area.y + area.height - 1,
            &status_line(self.snapshot),
        );

        let viewport_height = usize::from(area.height.saturating_sub(2));
        if viewport_height == 0 {
            return;
        }
        let lines = match self.state.page {
            DevTuiPage::Pipeline => pipeline_lines(self.snapshot),
            DevTuiPage::Release => release_lines(self.snapshot),
            DevTuiPage::Observations => observation_lines(self.snapshot),
        };
        let offset = self
            .state
            .vertical_offset
            .min(lines.len().saturating_sub(viewport_height));

        for (row, line) in lines.iter().skip(offset).take(viewport_height).enumerate() {
            let row = u16::try_from(row).expect("the row is bounded by a u16 terminal height");
            write_line(buffer, area, area.y + 1 + row, line);
        }
    }
}

/// Bottom line: what the session is holding and what quitting will do.
///
/// The held environment is named here because leaving the client tears it
/// down; a developer must not read the client's exit as cleanup that already
/// happened.
fn status_line(snapshot: &DevSnapshot) -> String {
    let held = match snapshot.runtime_endpoint() {
        Some(endpoint) => format!(
            "holding {} host={}",
            endpoint.base_url(),
            endpoint.route_host()
        ),
        None => "no activated environment".to_owned(),
    };
    format!("{held} | q tears down and exits | tab pages | up/down scrolls")
}

fn page_label(selected: DevTuiPage, page: DevTuiPage) -> String {
    if selected == page {
        format!("[{}]", page.as_str())
    } else {
        page.as_str().to_owned()
    }
}

fn write_line(buffer: &mut Buffer, area: Rect, row: u16, line: &str) {
    buffer.set_stringn(area.x, row, line, usize::from(area.width), Style::default());
}

fn pipeline_lines(snapshot: &DevSnapshot) -> Vec<String> {
    let mut lines = vec!["STAGES".to_owned()];
    for stage in snapshot.stages() {
        let state = match stage.state() {
            DevStageState::Awaiting => "awaiting".to_owned(),
            DevStageState::Running => "running".to_owned(),
            DevStageState::Passed => "passed".to_owned(),
            DevStageState::Failed(failure) => match failure.remedy() {
                Some(remedy) => format!(
                    "failed code={} detail={} remedy={remedy}",
                    failure.code(),
                    failure.detail(),
                ),
                None => format!(
                    "failed code={} detail={} remedy=-",
                    failure.code(),
                    failure.detail(),
                ),
            },
        };
        lines.push(format!("stage {} state={state}", stage.stage().as_str()));
    }

    lines.push(String::new());
    lines.push("GATE OUTCOMES".to_owned());
    if snapshot.gate_outcomes().is_empty() {
        lines.push("none".to_owned());
    } else {
        for outcome in snapshot.gate_outcomes() {
            let verdict = match outcome.verdict() {
                DevGateVerdict::Accepted(receipt) => format!(
                    "accepted report-id={} validated-draft-id={}",
                    receipt.report_id, receipt.validated_draft.validated_draft_id,
                ),
                DevGateVerdict::Refused(refusal) => {
                    format!("refused {}", exact_json(refusal))
                }
            };
            lines.push(format!(
                "package-id={} package-version={} wiring-id={} wiring-version={} verdict={verdict}",
                outcome.package_id(),
                outcome.package_version(),
                outcome.wiring_id(),
                outcome.wiring_version(),
            ));
        }
    }
    lines
}

fn release_lines(snapshot: &DevSnapshot) -> Vec<String> {
    let mut lines = vec!["RELEASE CARRIER".to_owned()];
    match snapshot.release() {
        Some(release) => {
            lines.push(format!("artifact-base={}", release.carrier().artifact_base));
            lines.push(format!(
                "manifest-digest={}",
                release.carrier().manifest_digest
            ));
            lines.push(String::new());
            lines.push("RELEASE".to_owned());
            lines.push(format!(
                "format-version={} tenant-id={} effective-release-id={} environment={}",
                release.manifest().format_version,
                release.manifest().release.tenant_id,
                release.manifest().release.effective_release_id.get(),
                release.manifest().release.environment,
            ));
        }
        None => lines.push("awaiting release".to_owned()),
    }

    lines.push(String::new());
    lines.push("RUNTIME ENDPOINT".to_owned());
    match snapshot.runtime_endpoint() {
        // Display only. The client never invokes through this endpoint; it
        // reads the read seam and nothing else.
        Some(endpoint) => lines.push(format!(
            "base-url={} route-host={}",
            endpoint.base_url(),
            endpoint.route_host()
        )),
        None => lines.push("awaiting activation".to_owned()),
    }

    lines.push(String::new());
    lines.push("MEMBERSHIPS".to_owned());
    let mut memberships = 0;
    for membership in snapshot.memberships() {
        memberships += 1;
        lines.push(format!(
            "package-id={} package-version={}",
            membership.package_id(),
            membership.package_version(),
        ));
    }
    if memberships == 0 {
        lines.push("none".to_owned());
    }

    lines.push(String::new());
    lines.push("OPERATIONS".to_owned());
    let mut operations = 0;
    for (component, token, facts) in snapshot.operations() {
        operations += 1;
        lines.push(format!(
            "package-id={} component={} interface-version={} digest={} token={} facts={}",
            component.package_id,
            component.component,
            component.interface_version,
            component.digest,
            token,
            exact_json(facts),
        ));
    }
    if operations == 0 {
        lines.push("none".to_owned());
    }

    lines.push(String::new());
    lines.push("ROUTES".to_owned());
    let mut routes = 0;
    for (id, attachment) in snapshot.routes() {
        routes += 1;
        lines.push(format!("id={id} facts={}", exact_json(attachment)));
    }
    if routes == 0 {
        lines.push("none".to_owned());
    }
    lines
}

fn observation_lines(snapshot: &DevSnapshot) -> Vec<String> {
    let mut lines = vec!["TRACES".to_owned()];
    if snapshot.traces().is_empty() {
        lines.push("none".to_owned());
    } else {
        for trace in snapshot.traces() {
            lines.push(format!(
                "trace-id={} root-service-name={} root-trace-name={} start-time-unix-nanos={} duration-nanos={}",
                trace.trace_id(),
                trace.root_service_name(),
                trace.root_trace_name(),
                trace.start_time_unix_nanos(),
                trace.duration().as_nanos(),
            ));
        }
    }

    lines.push(String::new());
    lines.push("TAPS".to_owned());
    if snapshot.taps().is_empty() {
        lines.push("none".to_owned());
    } else {
        for tap in snapshot.taps() {
            lines.push(format!(
                "subject={} record={}",
                tap.subject(),
                exact_json(tap.record()),
            ));
        }
    }
    lines
}

fn exact_json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).expect("read-model contract values always serialize")
}

/// What the driver must do after one key press.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevTuiAction {
    /// Navigation changed; repaint from the current snapshot.
    Redraw,
    /// Tear the environment down and leave the client.
    Quit,
    /// The key means nothing on this client.
    Ignore,
}

/// Apply one key press to `state` and report what the caller must do.
///
/// Pure: the mapping is asserted without a terminal, and no key reaches an
/// effect. Quitting is only ever a request the caller forwards to the session.
pub fn apply_key(state: &mut DevTuiState, key: KeyEvent) -> DevTuiAction {
    if key.kind == KeyEventKind::Release {
        return DevTuiAction::Ignore;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return DevTuiAction::Quit;
    }
    match key.code {
        KeyCode::Char('q' | 'Q') | KeyCode::Esc => return DevTuiAction::Quit,
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => state.next_page(),
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => state.previous_page(),
        KeyCode::Char('1') => state.select_page(DevTuiPage::Pipeline),
        KeyCode::Char('2') => state.select_page(DevTuiPage::Release),
        KeyCode::Char('3') => state.select_page(DevTuiPage::Observations),
        KeyCode::Down | KeyCode::Char('j') => state.scroll_down(1),
        KeyCode::Up | KeyCode::Char('k') => state.scroll_up(1),
        KeyCode::PageDown => state.scroll_down(PAGE_SCROLL_LINES),
        KeyCode::PageUp => state.scroll_up(PAGE_SCROLL_LINES),
        KeyCode::Home => state.scroll_up(usize::MAX),
        KeyCode::End => state.scroll_down(usize::MAX),
        _ => return DevTuiAction::Ignore,
    }
    DevTuiAction::Redraw
}

/// Run one development session under the interactive terminal client.
///
/// The twelve stages run exactly as the non-interactive command runs them. On
/// success the session then holds the activated environment open, because
/// asking for a terminal client is asking for a session to use; quitting
/// requests shutdown and the session leaves through its own native cleanup. A
/// stage failure ends the run, and the typed failure is reported after the
/// terminal is restored, as it is without this client.
pub async fn run(args: DevCommandArgs) -> anyhow::Result<()> {
    let session = DevSession::prepare(args).await?;
    let control = session.control();
    let mut subscription = session.read_handle().subscribe();
    let mut state = DevTuiState::default();
    let mut events = terminal::events();
    let mut screen =
        terminal::TerminalSession::enter().context("enter the interactive terminal")?;
    let mut session = pin!(session.run_until_shutdown());

    let mut driver_failure = None;
    paint(
        &mut screen,
        &subscription.current(),
        &state,
        &control,
        &mut driver_failure,
    );

    let mut reads_open = true;
    let mut events_open = true;
    let outcome = loop {
        tokio::select! {
            outcome = &mut session => break outcome,
            update = subscription.next(), if reads_open => match update {
                Some(snapshot) => {
                    paint(&mut screen, &snapshot, &state, &control, &mut driver_failure);
                }
                None => reads_open = false,
            },
            event = events.next(), if events_open => match event {
                Some(Ok(Event::Key(key))) => match apply_key(&mut state, key) {
                    DevTuiAction::Quit => control.request_shutdown(),
                    DevTuiAction::Redraw => paint(
                        &mut screen,
                        &subscription.current(),
                        &state,
                        &control,
                        &mut driver_failure,
                    ),
                    DevTuiAction::Ignore => {}
                },
                Some(Ok(Event::Resize(_, _))) => paint(
                    &mut screen,
                    &subscription.current(),
                    &state,
                    &control,
                    &mut driver_failure,
                ),
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    fail_driver(&mut driver_failure, error, &control);
                    events_open = false;
                }
                None => events_open = false,
            },
        }
    };

    drop(screen);
    let receipt = outcome?;
    if let Some(error) = driver_failure {
        return Err(anyhow::Error::new(error).context("drive the interactive terminal"));
    }
    if let Some(receipt) = receipt {
        print_receipt("run", &receipt);
    }
    Ok(())
}

/// Repaint unless the driver has already failed, and stop the session if it does.
fn paint(
    screen: &mut terminal::TerminalSession,
    snapshot: &DevSnapshot,
    state: &DevTuiState,
    control: &DevSessionControl,
    failure: &mut Option<io::Error>,
) {
    if failure.is_some() {
        return;
    }
    if let Err(error) = screen.draw(DevTui::new(snapshot, state)) {
        fail_driver(failure, error, control);
    }
}

/// Record the first driver failure and ask the session to shut down cleanly.
fn fail_driver(failure: &mut Option<io::Error>, error: io::Error, control: &DevSessionControl) {
    if failure.is_none() {
        *failure = Some(error);
    }
    control.request_shutdown();
}

/// The terminal itself: raw mode, the alternate screen, events, restoration.
///
/// Nothing here knows about the development loop. It is the repository's first
/// terminal driver and deliberately its own module so that the second one —
/// giving `wamn-receiving-tui` a binary — moves this out instead of writing a
/// second copy (wamn-10yt.5.9).
mod terminal {
    use std::io::{self, Stdout, Write as _};
    use std::panic;
    use std::sync::Once;

    use crossterm::ExecutableCommand as _;
    use crossterm::cursor::{Hide, Show};
    use crossterm::event::EventStream;
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use ratatui::widgets::Widget;

    /// Guards the one process-wide panic hook this driver installs.
    static PANIC_RESTORE: Once = Once::new();

    /// Keyboard, mouse and resize events from the entered terminal.
    pub(super) fn events() -> EventStream {
        EventStream::new()
    }

    /// An entered terminal, restored when it is dropped or the process panics.
    pub(super) struct TerminalSession {
        terminal: Terminal<CrosstermBackend<Stdout>>,
    }

    impl TerminalSession {
        /// Enter raw mode and the alternate screen.
        pub(super) fn enter() -> io::Result<Self> {
            PANIC_RESTORE.call_once(|| {
                let previous = panic::take_hook();
                panic::set_hook(Box::new(move |info| {
                    // A panic under raw mode otherwise leaves the developer
                    // with an unusable terminal and no visible message.
                    drop(restore());
                    previous(info);
                }));
            });
            enable_raw_mode()?;
            let mut output = io::stdout();
            output.execute(EnterAlternateScreen)?;
            output.execute(Hide)?;
            Ok(Self {
                terminal: Terminal::new(CrosstermBackend::new(output))?,
            })
        }

        /// Paint one widget over the whole terminal.
        pub(super) fn draw(&mut self, widget: impl Widget) -> io::Result<()> {
            self.terminal
                .draw(|frame| frame.render_widget(widget, frame.area()))?;
            Ok(())
        }
    }

    impl Drop for TerminalSession {
        fn drop(&mut self) {
            drop(restore());
        }
    }

    /// Leave the alternate screen and raw mode, in the reverse of entry order.
    fn restore() -> io::Result<()> {
        let mut output = io::stdout();
        output.execute(Show)?;
        output.execute(LeaveAlternateScreen)?;
        disable_raw_mode()?;
        output.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

    use crossterm::event::KeyEventState;
    use serde_json::json;
    use wamn_authoring_model::{GateReceipt, GateRefusal, ValidatedDraftRef};
    use wamn_catalog::{
        ArtifactHash, AttachmentKind, ComponentOperationDependency, DefinitionHash,
        EffectiveReleaseId, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION, ServingAttachment,
        ServingComponent, ServingComponentOperation, ServingManifest, ServingRelease,
        ServingWiring,
    };
    use wamn_runtime::plugins::wamn_jetstream::{
        RouterTapFormatVersion, RouterTapRecord, RouterTapRecordPhase, RouterTapSourceKind,
    };

    use super::*;
    use crate::dev::read::{
        DevGateOutcome, DevRuntimeEndpoint, DevTapObservation, DevTraceObservation,
        dev_read_channel,
    };
    use crate::dev::{DEV_STAGE_ORDER, DevStage, DevStageFailure};
    use crate::print_release_env::ReleaseCarrier;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn pipeline_renders_all_stages_and_typed_gate_verdicts() {
        let (publisher, handle) = dev_read_channel();
        publisher.stage_completed(DevStage::Migrate);
        publisher.stage_started(DevStage::Release);
        publisher.stage_failed(
            DevStage::Gate,
            DevStageFailure::new(
                "gate-unavailable",
                "Gate endpoint refused the document",
                Some("inspect the typed refusal"),
            ),
        );
        publisher.set_gate_outcomes(vec![
            gate_outcome(
                "accepted",
                DevGateVerdict::Accepted(GateReceipt {
                    report_id: "report-7".to_owned(),
                    validated_draft: ValidatedDraftRef {
                        validated_draft_id: DIGEST.to_owned(),
                    },
                }),
            ),
            gate_outcome(
                "authorization",
                DevGateVerdict::Refused(GateRefusal::AuthorizationDenied),
            ),
            gate_outcome(
                "contract-version",
                DevGateVerdict::Refused(GateRefusal::UnsupportedContractVersion {
                    requested: "2".to_owned(),
                    supported: "1".to_owned(),
                }),
            ),
            gate_outcome(
                "document",
                DevGateVerdict::Refused(GateRefusal::InvalidDocument {
                    detail: "missing entry".to_owned(),
                }),
            ),
            gate_outcome(
                "test-set",
                DevGateVerdict::Refused(GateRefusal::InvalidTestSet {
                    detail: "no cases".to_owned(),
                }),
            ),
            gate_outcome(
                "effects",
                DevGateVerdict::Refused(GateRefusal::EffectfulComponentReached {
                    components: vec!["payments".to_owned(), "ledger".to_owned()],
                }),
            ),
            gate_outcome(
                "command-id",
                DevGateVerdict::Refused(GateRefusal::CommandIdReuse),
            ),
        ]);

        let snapshot = handle.snapshot();
        let text = rendered_text(&snapshot, &DevTuiState::new(DevTuiPage::Pipeline), 320, 32);

        for stage in DEV_STAGE_ORDER {
            assert!(
                text.contains(&format!("stage {} state=", stage.as_str())),
                "{stage} is absent from:\n{text}"
            );
        }
        assert!(text.contains("state=passed"), "{text}");
        assert!(text.contains("state=running"), "{text}");
        assert!(text.contains("failed code=gate-unavailable"), "{text}");
        assert!(text.contains("remedy=inspect the typed refusal"), "{text}");
        assert!(
            text.contains("accepted report-id=report-7 validated-draft-id=sha256:"),
            "{text}"
        );
        for refusal in [
            r#"{"kind":"authorization-denied"}"#,
            r#"{"kind":"unsupported-contract-version","requested":"2","supported":"1"}"#,
            r#"{"kind":"invalid-document","detail":"missing entry"}"#,
            r#"{"kind":"invalid-test-set","detail":"no cases"}"#,
            r#"{"kind":"effectful-component-reached","components":["payments","ledger"]}"#,
            r#"{"kind":"command-id-reuse"}"#,
        ] {
            assert!(text.contains(refusal), "missing {refusal} from:\n{text}");
        }
    }

    #[test]
    fn release_renders_exact_carrier_memberships_operations_and_routes() {
        let (publisher, handle) = dev_read_channel();
        let manifest = manifest();
        let manifest_digest = manifest.digest();
        publisher.set_release(
            manifest,
            ReleaseCarrier {
                artifact_base: "registry.example.test/wamn/releases".to_owned(),
                manifest_digest: manifest_digest.clone(),
            },
        );

        let snapshot = handle.snapshot();
        let text = rendered_text(&snapshot, &DevTuiState::new(DevTuiPage::Release), 1000, 24);

        assert!(
            text.contains("artifact-base=registry.example.test/wamn/releases"),
            "{text}"
        );
        assert!(
            text.contains(&format!("manifest-digest={manifest_digest}")),
            "{text}"
        );
        assert!(text.contains("tenant-id=tenant-a"), "{text}");
        assert!(text.contains("effective-release-id=7"), "{text}");
        assert!(
            text.contains("package-id=receiving package-version=1.0.0"),
            "{text}"
        );
        assert!(text.contains("token=purchase-order/get"), "{text}");
        assert!(
            text.contains(r#""package":"inventory","version":"2.0.0""#),
            "{text}"
        );
        assert!(text.contains("id=purchase-order-http"), "{text}");
        assert!(text.contains("id=purchase-order-studio"), "{text}");
        assert!(
            text.contains(r#""host":"receiving.dev.localhost""#),
            "{text}"
        );
        assert!(text.contains(r#""kind":"http""#), "{text}");
        assert!(text.contains(r#""kind":"studio""#), "{text}");
    }

    #[test]
    fn observations_render_exact_trace_and_tap_facts() {
        let (publisher, handle) = dev_read_channel();
        publisher.merge_traces(vec![DevTraceObservation {
            trace_id: "0123456789abcdef".to_owned(),
            root_service_name: "wamn-host".to_owned(),
            root_trace_name: "component.invoke".to_owned(),
            start_time_unix_nanos: 1_725_000_000_000_000_000,
            duration: Duration::from_nanos(12_345),
        }]);
        publisher.push_tap(tap(
            "tap.tenant-a.project-a.dev.wiring.accepted",
            RouterTapRecordPhase::Accepted,
            RouterTapSourceKind::Attachment,
            None,
            None,
            json!({"purchase-order-id": "po-7"}),
        ));
        publisher.push_tap(tap(
            "tap.tenant-a.project-a.dev.wiring.settled",
            RouterTapRecordPhase::Settled,
            RouterTapSourceKind::Registration,
            Some("delivered"),
            Some(70_000),
            serde_json::Value::Null,
        ));

        let snapshot = handle.snapshot();
        let text = rendered_text(
            &snapshot,
            &DevTuiState::new(DevTuiPage::Observations),
            1000,
            12,
        );

        assert!(text.contains("trace-id=0123456789abcdef"), "{text}");
        assert!(text.contains("root-service-name=wamn-host"), "{text}");
        assert!(text.contains("root-trace-name=component.invoke"), "{text}");
        assert!(text.contains("duration-nanos=12345"), "{text}");
        assert!(
            text.contains("tap.tenant-a.project-a.dev.wiring.accepted"),
            "{text}"
        );
        assert!(text.contains(r#""format-version":1"#), "{text}");
        assert!(text.contains(r#""phase":"accepted""#), "{text}");
        assert!(text.contains(r#""phase":"settled""#), "{text}");
        assert!(text.contains(r#""source-kind":"attachment""#), "{text}");
        assert!(text.contains(r#""source-kind":"registration""#), "{text}");
        assert!(text.contains(r#""outcome":"delivered""#), "{text}");
        assert!(text.contains(r#""over-ceiling-bytes":70000"#), "{text}");
        assert!(text.contains(r#""redacted":true"#), "{text}");
        assert!(
            text.contains(r#""payload":{"purchase-order-id":"po-7"}"#),
            "{text}"
        );
    }

    #[test]
    fn page_navigation_resets_and_vertical_scrolling_reveals_later_rows() {
        let (_, handle) = dev_read_channel();
        let snapshot = handle.snapshot();
        let mut state = DevTuiState::default();

        let first = rendered_text(&snapshot, &state, 100, 6);
        assert!(first.contains("stage migrate"), "{first}");
        assert!(!first.contains("stage activate"), "{first}");

        state.scroll_down(usize::MAX);
        let last = rendered_text(&snapshot, &state, 100, 6);
        assert!(!last.contains("stage migrate"), "{last}");
        assert!(last.contains("stage activate"), "{last}");

        state.next_page();
        assert_eq!(state.page(), DevTuiPage::Release);
        assert_eq!(state.vertical_offset(), 0);
        let release = rendered_text(&snapshot, &state, 100, 6);
        assert!(release.contains("RELEASE CARRIER"), "{release}");

        state.previous_page();
        assert_eq!(state.page(), DevTuiPage::Pipeline);
        state.previous_page();
        assert_eq!(state.page(), DevTuiPage::Observations);
        assert_eq!(state.vertical_offset(), 0);
    }

    #[test]
    fn the_release_page_and_status_line_name_the_held_environment() {
        let (publisher, handle) = dev_read_channel();

        let awaiting = rendered_text(
            &handle.snapshot(),
            &DevTuiState::new(DevTuiPage::Release),
            120,
            24,
        );
        assert!(awaiting.contains("RUNTIME ENDPOINT"), "{awaiting}");
        assert!(awaiting.contains("awaiting activation"), "{awaiting}");
        assert!(awaiting.contains("no activated environment"), "{awaiting}");
        assert!(awaiting.contains("q tears down and exits"), "{awaiting}");

        publisher.set_runtime_endpoint(DevRuntimeEndpoint::new(
            "http://127.0.0.1:38080".to_owned(),
            "receiving.dev.localhost",
        ));
        let held = rendered_text(
            &handle.snapshot(),
            &DevTuiState::new(DevTuiPage::Release),
            120,
            24,
        );
        assert!(
            held.contains("base-url=http://127.0.0.1:38080 route-host=receiving.dev.localhost"),
            "{held}"
        );
        assert!(
            held.contains("holding http://127.0.0.1:38080 host=receiving.dev.localhost"),
            "{held}"
        );
        assert!(held.contains("q tears down and exits"), "{held}");
        assert!(!held.contains("awaiting activation"), "{held}");
    }

    #[test]
    fn the_status_line_survives_a_viewport_too_short_to_hold_content() {
        let (publisher, handle) = dev_read_channel();
        publisher.set_runtime_endpoint(DevRuntimeEndpoint::new(
            "http://127.0.0.1:38080".to_owned(),
            "receiving.dev.localhost",
        ));
        let snapshot = handle.snapshot();
        let state = DevTuiState::default();

        // Row equality, not containment: a viewport that runs one row long
        // overwrites the head of the status line and still contains its tail.
        let two_rows = rendered_text(&snapshot, &state, 200, 2);
        let rows = two_rows.lines().collect::<Vec<_>>();
        assert_eq!(rows.len(), 2, "{two_rows}");
        assert!(rows[0].starts_with("wamn dev |"), "{two_rows}");
        assert_eq!(
            rows[1],
            "holding http://127.0.0.1:38080 host=receiving.dev.localhost \
             | q tears down and exits | tab pages | up/down scrolls",
            "{two_rows}"
        );

        let one_row = rendered_text(&snapshot, &state, 200, 1);
        assert!(one_row.starts_with("wamn dev |"), "{one_row}");
        assert!(!one_row.contains("q tears down and exits"), "{one_row}");
    }

    #[test]
    fn keys_navigate_and_only_the_quit_keys_ask_for_shutdown() {
        let mut state = DevTuiState::default();

        for code in [KeyCode::Char('q'), KeyCode::Char('Q'), KeyCode::Esc] {
            assert_eq!(
                apply_key(&mut state, press(code, KeyModifiers::NONE)),
                DevTuiAction::Quit,
                "{code:?} must quit"
            );
        }
        assert_eq!(
            apply_key(&mut state, press(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            DevTuiAction::Quit
        );
        assert_eq!(state.page(), DevTuiPage::Pipeline, "quitting moves no page");

        assert_eq!(
            apply_key(&mut state, press(KeyCode::Tab, KeyModifiers::NONE)),
            DevTuiAction::Redraw
        );
        assert_eq!(state.page(), DevTuiPage::Release);
        apply_key(&mut state, press(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(state.page(), DevTuiPage::Pipeline);
        apply_key(&mut state, press(KeyCode::Char('3'), KeyModifiers::NONE));
        assert_eq!(state.page(), DevTuiPage::Observations);

        apply_key(&mut state, press(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.vertical_offset(), 1);
        apply_key(&mut state, press(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(state.vertical_offset(), 1 + PAGE_SCROLL_LINES);
        apply_key(&mut state, press(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(state.vertical_offset(), PAGE_SCROLL_LINES);
        apply_key(&mut state, press(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(state.vertical_offset(), 0);
        apply_key(&mut state, press(KeyCode::Char('1'), KeyModifiers::NONE));
        assert_eq!(state.page(), DevTuiPage::Pipeline);

        assert_eq!(
            apply_key(&mut state, press(KeyCode::Char('z'), KeyModifiers::NONE)),
            DevTuiAction::Ignore
        );
        assert_eq!(
            apply_key(
                &mut state,
                KeyEvent {
                    code: KeyCode::Char('q'),
                    modifiers: KeyModifiers::NONE,
                    kind: KeyEventKind::Release,
                    state: KeyEventState::NONE,
                }
            ),
            DevTuiAction::Ignore,
            "a key release must not tear the environment down"
        );
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn gate_outcome(wiring_id: &str, verdict: DevGateVerdict) -> DevGateOutcome {
        DevGateOutcome {
            package_id: "receiving".to_owned(),
            package_version: "1.0.0".to_owned(),
            wiring_id: wiring_id.to_owned(),
            wiring_version: 1,
            verdict,
        }
    }

    fn manifest() -> ServingManifest {
        let package = PackageCoordinate::new("receiving", "1.0.0").expect("valid package");
        let operation = ServingComponentOperation {
            registered_operation: Some("receiving:purchase-order/get@1.0.0".to_owned()),
            dependencies: vec![ComponentOperationDependency {
                package: "inventory".to_owned(),
                version: "2.0.0".to_owned(),
                digest: DIGEST.to_owned(),
                operation: "inventory:item/get@2.0.0".to_owned(),
            }],
            statements: BTreeMap::new(),
        };
        let component = ServingComponent {
            package_id: "receiving".to_owned(),
            component: "receiving".to_owned(),
            interface_version: "0.1.0".to_owned(),
            digest: ArtifactHash::parse(DIGEST).expect("valid digest"),
            operations: BTreeMap::from([("purchase-order/get".to_owned(), operation)]),
        };
        let http = attachment(
            AttachmentKind::Http,
            "purchase-order-http",
            "/purchase-order",
        );
        let studio = attachment(
            AttachmentKind::Studio,
            "purchase-order-studio",
            "/studio/purchase-order",
        );
        ServingManifest {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                tenant_id: "tenant-a".to_owned(),
                effective_release_id: EffectiveReleaseId::new(7).expect("valid release"),
                environment: "dev".to_owned(),
                packages: BTreeSet::from([package]),
            },
            components: BTreeSet::from([component]),
            wirings: BTreeSet::from([ServingWiring {
                package_id: "receiving".to_owned(),
                wiring_id: "purchase-order".to_owned(),
                wiring_version: 1,
                graph_hash: DefinitionHash::parse(DIGEST).expect("valid digest"),
            }]),
            attachments: BTreeMap::from([
                ("purchase-order-http".to_owned(), http),
                ("purchase-order-studio".to_owned(), studio),
            ]),
            registrations: BTreeMap::new(),
        }
    }

    fn attachment(kind: AttachmentKind, id: &str, path: &str) -> ServingAttachment {
        let kind_name = match kind {
            AttachmentKind::Http => "http",
            AttachmentKind::Internal => "internal",
            AttachmentKind::Studio => "studio",
            AttachmentKind::Cron => "cron",
        };
        let definition = json!({
            "id": id,
            "kind": kind_name,
            "route": {
                "host": "receiving.dev.localhost",
                "method": "POST",
                "path": path,
            },
        });
        let definition_hash =
            DefinitionHash::parse(wamn_execution_contract::canonical_json_sha256(&definition))
                .expect("the canonicalizer emits a valid definition hash");
        ServingAttachment {
            kind,
            package_id: "receiving".to_owned(),
            wiring_id: "purchase-order".to_owned(),
            wiring_version: 1,
            definition_hash,
            definition,
            auth_policy: json!({"mode": "pat"}),
            registered_operation: Some("receiving:purchase-order/get@1.0.0".to_owned()),
        }
    }

    fn tap(
        subject: &str,
        phase: RouterTapRecordPhase,
        source_kind: RouterTapSourceKind,
        outcome: Option<&str>,
        over_ceiling_bytes: Option<u64>,
        payload: serde_json::Value,
    ) -> DevTapObservation {
        DevTapObservation {
            subject: subject.to_owned(),
            record: RouterTapRecord {
                delivery_id: "delivery-7".into(),
                format_version: RouterTapFormatVersion::V1,
                outcome: outcome.map(Into::into),
                over_ceiling_bytes,
                payload,
                phase,
                redacted: true,
                source_id: "purchase-order-http".into(),
                source_kind,
                wiring_id: "purchase-order".into(),
                wiring_version: 1,
            },
        }
    }

    fn rendered_text(
        snapshot: &DevSnapshot,
        state: &DevTuiState,
        width: u16,
        height: u16,
    ) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        DevTui::new(snapshot, state).render(area, &mut buffer);
        (0..height)
            .map(|row| {
                let mut line = String::new();
                for column in 0..width {
                    line.push_str(buffer[(column, row)].symbol());
                }
                line.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
