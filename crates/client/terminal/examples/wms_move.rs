//! Bind a generated pallet read to the generated move form.

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use wamn_client::{ClientError, HttpResponse};
use wamn_client_terminal::operator::{
    Action, Application, ExitReason, GeneratedApplication, PreparedRequest, run_application,
};
use wamn_client_tui::screen::IntentValues;
use wamn_client_tui::submission::{Attempt, SessionBinding, State};
use wamn_generated_wms_tui::screens::{inventory, pallet};

const PALLET: usize = 0;
const MOVE: usize = 1;

#[derive(Debug)]
struct MoveApplication {
    generated: GeneratedApplication,
}

impl MoveApplication {
    fn new(label: &str, binding: SessionBinding) -> Self {
        let mut generated = GeneratedApplication::new(
            label,
            vec![pallet::get(binding.clone()), inventory::r#move(binding)],
        );
        generated
            .edit_field(PALLET, "/id")
            .expect("the generated pallet read declares an editable ID");
        Self { generated }
    }

    fn compose_binding(&mut self) -> Result<(), String> {
        let read = self.generated.screen(PALLET);
        let command = self.generated.screen(MOVE);
        if !read.submission().available()
            || read.submission().binding() != command.submission().binding()
            || !matches!(read.submission().state(), State::Succeeded { .. })
        {
            return Err("Load the pallet from the same active target before moving it.".into());
        }
        let row = read
            .rows()
            .first()
            .ok_or("The pallet read returned no row.")?;
        let requested = read
            .submission()
            .captured()
            .and_then(|request| request.item().get("id"));
        if requested != row.get("id") || requested.is_none() {
            return Err("The returned pallet does not match the requested pallet.".into());
        }
        let id = row["id"].clone();
        let revision = row["row_version"].clone();
        let command = self.generated.screen_mut(MOVE);
        command
            .bind("/value/pallet_id", id)
            .and_then(|()| command.bind("/value/expected_row_version", revision))
            .and_then(|()| command.mark_composed())
            .map_err(|error| error.to_string())?;
        self.generated.edit_field(MOVE, "/value/to_location_id")
    }
}

impl Application for MoveApplication {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        self.generated.render(area, buffer);
    }

    fn key(&mut self, key: KeyEvent) -> Action {
        self.generated.key(key)
    }

    fn prepare(
        &mut self,
        action: Action,
        intent: Option<&IntentValues>,
    ) -> Result<PreparedRequest, String> {
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
        if screen == PALLET
            && was_pending
            && matches!(
                self.generated.screen(PALLET).submission().state(),
                State::Succeeded { .. }
            )
            && let Err(error) = self.compose_binding()
        {
            self.generated.set_message(error);
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

#[tokio::main]
async fn main() -> Result<ExitReason, Box<dyn std::error::Error>> {
    run_application("WMS move", MoveApplication::new).await
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crossterm::event::{KeyCode, KeyModifiers};
    use serde_json::{Value, json};
    use wamn_client::{HttpRequest, StaticPat, Transport, WamnClient};
    use wamn_client_tui::draft::FieldState;
    use wamn_client_tui::screen::Availability;

    use super::*;

    const PALLET_ID: &str = "33333333-0000-0000-0000-000000000002";
    const DESTINATION: &str = "33333333-0000-0000-0000-000000000003";
    const LABEL_KEY: &str = "labels/move-9.zpl";

    fn binding() -> SessionBinding {
        SessionBinding {
            url: "http://wms.test".into(),
            host: None,
            target_instance: "target-1".into(),
        }
    }

    fn intent(id: &str) -> IntentValues {
        IntentValues {
            request_id: id.into(),
            idempotency_key: format!("idem-{id}"),
            occurred_at: "2026-09-10T10:00:00Z".into(),
        }
    }

    fn send(screen: usize, retry: bool) -> Action {
        Action::Send {
            screen,
            retry,
            delete_confirmed: false,
        }
    }

    fn response(status: u16, body: &Value) -> Result<HttpResponse, ClientError> {
        Ok(HttpResponse {
            status,
            body: body.to_string(),
        })
    }

    fn pallet_row() -> Value {
        json!({
            "id": PALLET_ID, "location_id": "33333333-0000-0000-0000-000000000001",
            "pallet_code": "P-2", "row_version": 7, "status": "available",
            "created_at": "2026-09-10T10:00:00Z", "updated_at": "2026-09-10T10:00:00Z"
        })
    }

    fn movement() -> Value {
        json!({
            "movement_id": "33333333-0000-0000-0000-000000000009", "pallet_id": PALLET_ID,
            "location_id": DESTINATION, "pallet_status": "available", "row_version": 8
        })
    }

    fn pending_read() -> (MoveApplication, PreparedRequest) {
        let mut app = MoveApplication::new("WMS move", binding());
        for character in PALLET_ID.chars() {
            app.key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let request = app
            .prepare(send(PALLET, false), Some(&intent("read")))
            .unwrap();
        (app, request)
    }

    fn ready() -> MoveApplication {
        let (mut app, request) = pending_read();
        app.resolve(
            PALLET,
            request.attempt,
            response(200, &json!([{"request_id": "read", "value": pallet_row()}])),
        );
        assert_eq!(app.generated.active_screen(), Some(MOVE));
        for character in DESTINATION.chars() {
            app.key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app
    }

    fn render(app: &MoveApplication) -> String {
        let area = Rect::new(0, 0, 180, 60);
        let mut buffer = Buffer::empty(area);
        app.render(area, &mut buffer);
        buffer.content().iter().map(|cell| cell.symbol()).collect()
    }

    fn assert_spent(app: &mut MoveApplication) {
        assert_eq!(
            app.generated.screen(MOVE).availability(),
            Availability::Spent
        );
        for key in [
            KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        ] {
            let action = app.key(key);
            assert!(app.prepare(action, Some(&intent("again"))).is_err());
        }
        assert!(matches!(app.next_action(), Action::None));
    }

    #[derive(Debug, Default)]
    struct RecordingTransport(Mutex<Vec<HttpRequest>>);

    #[async_trait::async_trait]
    impl Transport for RecordingTransport {
        async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
            self.0.lock().unwrap().push(request);
            let mut value = movement();
            value["zpl"] = json!("^XA^XZ");
            value["stored"] = json!({"container": "labels", "key": LABEL_KEY});
            response(200, &json!([{"request_id": "move", "value": value}]))
        }
    }

    #[tokio::test]
    async fn the_bound_move_sends_exact_bytes_and_displays_the_label_key() {
        let mut app = ready();
        for pointer in ["/value/pallet_id", "/value/expected_row_version"] {
            assert!(
                app.generated
                    .screen_mut(MOVE)
                    .edit(pointer, FieldState::Value(json!(99)))
                    .is_err()
            );
        }
        assert!(
            app.generated
                .edit_field(MOVE, "/value/expected_row_version")
                .is_err()
        );
        let request = app
            .prepare(send(MOVE, false), Some(&intent("move")))
            .unwrap();
        let transport = Arc::new(RecordingTransport::default());
        let client = WamnClient::new(
            "http://wms.test",
            None,
            Arc::new(StaticPat::new("pat-test").unwrap()),
            transport.clone(),
        );
        let result = client
            .submit(&request.route, &request.parameters, &request.body)
            .await;
        app.resolve(MOVE, request.attempt, result);
        let sent = transport.0.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].body, br#"[{"request_id":"move","value":{"expected_row_version":7,"idempotency_key":"idem-move","occurred_at":"2026-09-10T10:00:00.000000Z","pallet_id":"33333333-0000-0000-0000-000000000002","to_location_id":"33333333-0000-0000-0000-000000000003"}}]"#);
        assert!(matches!(
            app.generated.screen(MOVE).submission().state(),
            State::Succeeded { .. }
        ));
        assert!(render(&app).contains(LABEL_KEY));
        assert_spent(&mut app);
    }

    #[test]
    fn incompatible_or_mismatched_reads_do_not_unlock_the_move() {
        for (request_id, path, replacement) in [
            ("read", "/id", json!(DESTINATION)),
            ("read", "/row_version", json!("invalid")),
            ("other-read", "/row_version", json!(7)),
        ] {
            let (mut app, request) = pending_read();
            let mut row = pallet_row();
            *row.pointer_mut(path).unwrap() = replacement;
            app.resolve(
                PALLET,
                request.attempt,
                response(200, &json!([{"request_id": request_id, "value": row}])),
            );
            assert_eq!(app.generated.active_screen(), Some(PALLET));
            assert_eq!(
                app.generated.screen(MOVE).availability(),
                Availability::RequiresComposition
            );
            assert!(
                app.prepare(send(MOVE, false), Some(&intent("move")))
                    .is_err()
            );
        }
    }

    #[test]
    fn changed_or_invalidated_sessions_do_not_accept_old_read_bindings() {
        for invalidate in [false, true] {
            let (mut app, request) = pending_read();
            if invalidate {
                app.generated.screen_mut(PALLET).invalidate();
                app.generated.screen_mut(MOVE).invalidate();
            } else {
                app.generated.screen_mut(MOVE).activate(SessionBinding {
                    target_instance: "replacement".into(),
                    ..binding()
                });
            }
            app.resolve(
                PALLET,
                request.attempt,
                response(200, &json!([{"request_id": "read", "value": pallet_row()}])),
            );
            assert_eq!(app.generated.active_screen(), Some(PALLET));
            assert!(
                app.prepare(send(MOVE, false), Some(&intent("move")))
                    .is_err()
            );
        }
    }

    #[test]
    fn the_declared_partial_response_preserves_the_commit_and_spends_the_move() {
        let mut app = ready();
        let request = app
            .prepare(send(MOVE, false), Some(&intent("move")))
            .unwrap();
        let failed = json!({"code": "write_failed", "operation": "label-store", "effect_outcome": "responded"});
        let body = json!({
            "committed_result": [{"request_id": "move", "value": movement()}],
            "failed_outcome": failed
        });
        app.resolve(MOVE, request.attempt, response(500, &body));
        assert_eq!(
            app.generated.screen(MOVE).submission().state(),
            &State::PartiallyCompleted {
                committed_result: movement(),
                failed_outcome: failed
            }
        );
        let shown = render(&app);
        assert!(shown.contains("committed work remains"));
        assert!(shown.contains("write_failed"));
        assert!(shown.contains("33333333-0000-0000-0000-000000000009"));
        assert_spent(&mut app);
    }

    #[test]
    fn missing_commit_evidence_keeps_the_move_uncertain_without_replay() {
        let mut app = ready();
        let request = app
            .prepare(send(MOVE, false), Some(&intent("move")))
            .unwrap();
        app.resolve(
            MOVE,
            request.attempt,
            response(
                500,
                &json!({
                    "failed_outcome": {"code": "write_failed"}
                }),
            ),
        );
        assert!(matches!(
            app.generated.screen(MOVE).submission().state(),
            State::Uncertain { .. }
        ));
        assert!(render(&app).contains("Outcome unknown"));
        assert!(app.prepare(send(MOVE, true), None).is_err());
        assert!(
            app.prepare(send(MOVE, false), Some(&intent("again")))
                .is_err()
        );
        assert!(matches!(app.next_action(), Action::None));
    }
}
