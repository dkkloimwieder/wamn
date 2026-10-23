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
fn the_record_carries_every_pattern_field_as_text() {
    let pattern = wamn_number_pattern::parse("{field:facility}-{seq:n,reset=year}").unwrap();
    let record = |pairs: &[(&str, Option<&str>)]| {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.map(str::to_owned)))
            .collect::<BTreeMap<_, _>>()
    };
    let full = record(&[
        ("facility", Some("DAY")),
        ("year", Some("26")),
        ("extra", None),
    ]);
    let fields = record_fields(&pattern, &full).unwrap();
    assert_eq!(fields["facility"], "DAY");
    assert_eq!(fields["year"], "26");
    let code = |error: StatementError| match error {
        StatementError::Postgres(PgError::QueryError((code, _))) => code,
        other => panic!("a query error: {other:?}"),
    };
    let null = record(&[("facility", None), ("year", Some("26"))]);
    assert_eq!(
        code(record_fields(&pattern, &null).unwrap_err()),
        NULL_FIELD
    );
    let missing = record(&[("facility", Some("DAY"))]);
    assert_eq!(
        code(record_fields(&pattern, &missing).unwrap_err()),
        INVALID_INPUT
    );
}

mod live;
