use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use wamn_execution_contract::canonical_json_bytes;
use wamn_platform_identity::IdentityRefusal;
use wamn_platform_identity::paging::{ListPage, Order};

fn scope() -> Value {
    json!({"principal": "00000000-0000-0000-0000-000000000001", "org": "acme", "project": null})
}

#[test]
fn control_lists_first_next_last_and_legacy() {
    for (operation, order, keys) in [
        (
            "project.list",
            Order::Project,
            vec![json!(["a"]), json!(["b"]), json!(["c"])],
        ),
        (
            "control.mine",
            Order::Project,
            vec![json!(["a"]), json!(["b"]), json!(["c"])],
        ),
        (
            "environment.list",
            Order::Environment,
            vec![json!(["a"]), json!(["b"]), json!(["c"])],
        ),
        (
            "user.list",
            Order::Principal,
            vec![
                json!(["a", "00000000-0000-0000-0000-000000000001"]),
                json!(["b", "00000000-0000-0000-0000-000000000002"]),
                json!(["c", "00000000-0000-0000-0000-000000000003"]),
            ],
        ),
        (
            "member.list",
            Order::Principal,
            vec![
                json!(["a", "00000000-0000-0000-0000-000000000001"]),
                json!(["b", "00000000-0000-0000-0000-000000000002"]),
                json!(["c", "00000000-0000-0000-0000-000000000003"]),
            ],
        ),
    ] {
        let first = ListPage::new(&json!({"limit":1}), operation, scope(), order).unwrap();
        assert_eq!(first.read_limit(), Some(2));
        assert_eq!(first.after(0), None);
        let answer = first.answer("rows", keys[..2].to_vec(), Clone::clone);
        assert_eq!(answer["rows"], json!([keys[0]]));
        assert!(answer["total"].is_null());
        let next = ListPage::new(
            &json!({"limit":1,"cursor":answer["next_cursor"]}),
            operation,
            scope(),
            order,
        )
        .unwrap();
        assert_eq!(next.after(0), Some("a"));
        let answer = next.answer("rows", keys[1..].to_vec(), Clone::clone);
        assert_eq!(answer["rows"], json!([keys[1]]));
        let last = ListPage::new(
            &json!({"cursor":answer["next_cursor"]}),
            operation,
            scope(),
            order,
        )
        .unwrap();
        assert_eq!(last.read_limit(), Some(101));
        assert_eq!(last.after(0), Some("b"));
        let answer = last.answer("rows", vec![keys[2].clone()], Clone::clone);
        assert!(answer["next_cursor"].is_null());
        assert!(answer["total"].is_null()); // A suffix does not establish the full count.
        let all = ListPage::new(&json!({}), operation, scope(), order).unwrap();
        assert_eq!(all.read_limit(), None);
        let many = all.answer("rows", (0..101).collect::<Vec<_>>(), |row| json!([row]));
        assert_eq!(many["rows"].as_array().unwrap().len(), 101);
        assert_eq!(many["total"], 101);
        let answer = all.answer("rows", keys.clone(), Clone::clone);
        assert_eq!(answer["rows"], json!(keys));
        assert_eq!(answer["total"], 3);
        assert!(answer["next_cursor"].is_null());
        let first = ListPage::new(&json!({"limit":3}), operation, scope(), order).unwrap();
        assert_eq!(first.answer("rows", keys, Clone::clone)["total"], 3);
        assert_eq!(
            first.answer("rows", Vec::<Value>::new(), Clone::clone)["total"],
            0
        );
    }
}

#[test]
fn refuses_bad_limits_and_cursors() {
    let parse = |input: Value| ListPage::new(&input, "project.list", scope(), Order::Project);
    for limit in [
        json!(null),
        json!(0),
        json!(-1),
        json!(101),
        json!(2_147_483_648_i64),
        json!(1.5),
        json!("25"),
        json!(true),
    ] {
        assert_eq!(
            parse(json!({"limit":limit})).unwrap_err().refusal(),
            Some(IdentityRefusal::Invalid("limit"))
        );
    }
    for cursor in [json!(null), json!(""), json!("???"), json!(5)] {
        assert_eq!(
            parse(json!({"cursor":cursor})).unwrap_err().refusal(),
            Some(IdentityRefusal::Invalid("cursor"))
        );
    }
    let answer = parse(json!({"limit":1}))
        .unwrap()
        .answer("rows", vec!["a", "b"], |key| json!([key]));
    let encoded = answer["next_cursor"].as_str().unwrap();
    let bytes = URL_SAFE_NO_PAD.decode(encoded).unwrap();
    let original: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(bytes, canonical_json_bytes(&original));
    for (field, value) in [
        ("v", json!(2)),
        ("operation", json!("control.mine")),
        ("scope", json!({})),
        ("order", json!("Environment")),
        ("key", json!([1])),
        ("key", json!([])),
        ("extra", json!(true)),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        let cursor = URL_SAFE_NO_PAD.encode(canonical_json_bytes(&changed));
        assert_eq!(
            parse(json!({"cursor":cursor})).unwrap_err().refusal(),
            Some(IdentityRefusal::Invalid("cursor"))
        );
    }
    for cursor in [
        format!("{encoded}="),
        URL_SAFE_NO_PAD.encode(serde_json::to_string_pretty(&original).unwrap()),
    ] {
        assert!(parse(json!({"cursor":cursor})).is_err());
    }
    assert!(
        ListPage::new(
            &json!({"cursor":encoded}),
            "project.list",
            json!({"principal":"other", "org":"acme", "project":null}),
            Order::Project
        )
        .is_err()
    );
}
