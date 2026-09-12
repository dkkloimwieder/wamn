use wamn_client::descriptor::FieldSchema;
use wamn_client::FieldDescriptor;
use wamn_client_tui::screen::{Availability, Screen};
use wamn_client_tui::submission::SessionBinding;

fn binding(instance: &str) -> SessionBinding {
    SessionBinding {
        url: "http://127.0.0.1:31001".to_owned(),
        host: Some("scaffold.localhost".to_owned()),
        target_instance: instance.to_owned(),
    }
}

fn selected_screen() -> Screen {
    wamn_receiving_ui::screens(binding("first"))
        .into_iter()
        .find(|screen| screen.spec().operation == "wamn-receiving:receiving/record-receipt@1.0.0")
        .expect("the custom composition retains its selected operation")
}

fn assert_declared_interaction(mut screen: Screen) {
    assert_eq!(screen.spec().operation, "wamn-receiving:receiving/record-receipt@1.0.0");
    assert_eq!(screen.spec().kind, "command");
    assert!(screen.submission().available());
    assert!(!screen.activate(binding("first")));
    screen.invalidate();
    assert_eq!(screen.availability(), Availability::Unavailable);
    assert!(screen.activate(binding("second")));
    assert!(screen.rows().is_empty());
    assert!(screen.cursor().is_none());
    assert!(!screen.dirty());
}

#[test]
fn scaffold_tracks_contract() {
    assert_declared_interaction(selected_screen());
}

#[test]
fn a_changed_declared_kind_fails_the_interaction_assertion() {
    let mut spec = *selected_screen().spec();
    spec.kind = if spec.kind == "delete" { "query" } else { "delete" };
    spec.response.kind = spec.kind;
    let changed = Screen::new(Box::leak(Box::new(spec)), binding("first"));
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_declared_interaction(changed);
    })).is_err());
}

#[test]
fn an_unasserted_additive_result_field_is_allowed() {
    let mut spec = *selected_screen().spec();
    let mut fields = spec.response.fields.to_vec();
    fields.push(FieldSchema {
        field: FieldDescriptor {
            path: "scaffold_additive_example",
            type_name: "text",
            nullable: true,
            values: &[],
        },
        required: false,
        children: &[],
        minimum: None,
        maximum: None,
    });
    spec.response.fields = Box::leak(fields.into_boxed_slice());
    assert_declared_interaction(Screen::new(Box::leak(Box::new(spec)), binding("first")));
}
