use super::*;

fn counter(pattern: &str) -> Counter {
    wamn_number_pattern::parse(pattern)
        .unwrap()
        .counters()
        .next()
        .unwrap()
        .clone()
}

#[test]
fn a_counter_renders_padded_and_never_truncated() {
    assert_eq!(render_counter(&counter("{seq:n}"), 17), "17");
    assert_eq!(render_counter(&counter("{seq:n,width=8}"), 17), "00000017");
    assert_eq!(render_counter(&counter("{seq:n,06}"), 17), "000017");
    assert_eq!(
        render_counter(&counter("{seq:n,width=6,pad=_}"), 17),
        "____17"
    );
    assert_eq!(render_counter(&counter("{seq:n,hex}"), 17), "11");
    assert_eq!(render_counter(&counter("{seq:n,hex,width=4}"), 255), "00FF");
    assert_eq!(render_counter(&counter("{seq:n,width=2}"), 12345), "12345");
}

#[test]
fn counters_take_name_order_after_the_counter_their_key_names() {
    let order = |pattern: &str| {
        let pattern = wamn_number_pattern::parse(pattern).unwrap();
        counter_order(&pattern)
            .into_iter()
            .map(|counter| counter.name().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        order("B{seq:batch,per=batch_id}-{seq:line,reset=batch}"),
        ["batch", "line"]
    );
    assert_eq!(order("{seq:b}{seq:a}{seq:c}"), ["a", "b", "c"]);
    assert_eq!(
        order("{seq:z,per=k}{seq:a,reset=z}{seq:m}"),
        ["m", "z", "a"]
    );
}

#[test]
fn the_keys_carry_every_value_the_pattern_names_as_text() {
    let pattern = wamn_number_pattern::parse("{field:facility}-{seq:n,reset=year}").unwrap();
    let keys = |pairs: &[(&str, Option<&str>)]| {
        key_map(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.map(str::to_owned)))
                .collect(),
        )
    };
    let full = keys(&[
        ("facility", Some("DAY")),
        ("year", Some("26")),
        ("extra", None),
    ])
    .unwrap();
    let values = key_values(&pattern, &full).unwrap();
    assert_eq!(values["facility"], "DAY");
    assert_eq!(values["year"], "26");
    assert!(
        !values.contains_key("extra"),
        "a key the pattern does not name is not read"
    );
    let code = |error: Failure| match statement_failure(error) {
        StatementError::Postgres(PgError::QueryError((code, _))) => code,
        other => panic!("a query error: {other:?}"),
    };
    let null = keys(&[("facility", None), ("year", Some("26"))]).unwrap();
    assert_eq!(code(key_values(&pattern, &null).unwrap_err()), NULL_FIELD);
    let empty = keys(&[("facility", Some("")), ("year", Some("26"))]).unwrap();
    assert_eq!(
        code(key_values(&pattern, &empty).unwrap_err()),
        INVALID_INPUT
    );
    let missing = keys(&[("facility", Some("DAY"))]).unwrap();
    assert_eq!(
        code(key_values(&pattern, &missing).unwrap_err()),
        INVALID_INPUT
    );
    let twice = keys(&[("facility", Some("DAY")), ("facility", Some("CIN"))]).unwrap_err();
    assert_eq!(code(twice), INVALID_INPUT);
    // A bare pattern takes no keys.
    let bare = wamn_number_pattern::parse("P-{seq:po}").unwrap();
    assert!(key_values(&bare, &keys(&[]).unwrap()).unwrap().is_empty());
}

#[test]
fn the_date_formats_name_every_date_token_and_date_key() {
    let formats = |pattern: &str| {
        date_formats(&wamn_number_pattern::parse(pattern).unwrap()).collect::<Vec<_>>()
    };
    assert_eq!(
        formats("{date:YYYY}-{date:WW}-{seq:n,reset=date:YY}"),
        [DateFormat::Year, DateFormat::Week, DateFormat::ShortYear]
    );
    assert!(formats("P-{seq:po}-{field:dock}").is_empty());
}

#[test]
fn a_refusal_keeps_its_cause_until_the_statement_boundary() {
    let source = wamn_number_pattern::parse("{seq:").unwrap_err();
    let message = format!("the pattern of number sequence purchase_order does not parse: {source}");
    let error = caused_refusal(INVALID_INPUT, message.clone(), source);
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<wamn_number_pattern::PatternError>()
    );
    match statement_failure(error) {
        StatementError::Postgres(PgError::QueryError((code, text))) => {
            assert_eq!(code, INVALID_INPUT);
            assert_eq!(text, message);
        }
        other => panic!("the pattern refusal stays a query error: {other:?}"),
    }
}

mod live;
