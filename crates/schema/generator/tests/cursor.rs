use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use wamn_catalog::manifest::CursorDirection;
use wamn_catalog::schema::ColumnType;
use wamn_schema_generator::{CursorErrorType, CursorV1, CursorValue, decode_cursor, encode_cursor};

const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

#[test]
fn whole_second_timestamp_has_one_literal_wire_spelling() {
    let cursor = CursorV1::new(
        "created_at",
        CursorDirection::Ascending,
        CursorValue::Timestamptz("2026-08-29T12:34:56.000000Z".into()),
        ID,
    );
    let encoded = encode_cursor(&cursor).unwrap();

    assert_eq!(
        URL_SAFE_NO_PAD.decode(&encoded).unwrap(),
        br#"{"direction":"ascending","field":"created_at","id":"01234567-89ab-cdef-0123-456789abcdef","key":"2026-08-29T12:34:56.000000Z","v":1}"#
    );
    assert_eq!(
        decode_cursor(
            &encoded,
            "created_at",
            CursorDirection::Ascending,
            ColumnType::Timestamptz,
        )
        .unwrap(),
        cursor
    );
}

#[test]
fn numeric_cursor_preserves_postgresql_lexical_scale() {
    let cursor = CursorV1::new(
        "amount",
        CursorDirection::Descending,
        CursorValue::Numeric("12.3400".into()),
        ID,
    );
    let encoded = encode_cursor(&cursor).unwrap();

    assert_eq!(
        decode_cursor(
            &encoded,
            "amount",
            CursorDirection::Descending,
            ColumnType::Numeric,
        )
        .unwrap()
        .key(),
        &CursorValue::Numeric("12.3400".into())
    );
}

#[test]
fn malformed_or_mismatched_cursor_is_typed_invalid_input() {
    let valid = encode_cursor(&CursorV1::new(
        "created_at",
        CursorDirection::Ascending,
        CursorValue::Timestamptz("2026-08-29T12:34:56.123456Z".into()),
        ID,
    ))
    .unwrap();

    let cases = [
        (
            "not-base64!".to_owned(),
            "created_at",
            CursorDirection::Ascending,
        ),
        (
            format!("{valid}="),
            "created_at",
            CursorDirection::Ascending,
        ),
        (valid.clone(), "status", CursorDirection::Ascending),
        (valid.clone(), "created_at", CursorDirection::Descending),
    ];
    for (encoded, field, direction) in cases {
        let error = decode_cursor(&encoded, field, direction, ColumnType::Timestamptz).unwrap_err();
        assert_eq!(error.error_type(), CursorErrorType::InvalidInput);
    }
}

#[test]
fn noncanonical_json_and_unknown_version_never_reset_to_first_page() {
    let payloads = [
        br#"{"field":"created_at","v":1,"direction":"ascending","key":"2026-08-29T12:34:56.000000Z","id":"01234567-89ab-cdef-0123-456789abcdef"}"#.as_slice(),
        br#"{"v":1, "field":"created_at","direction":"ascending","key":"2026-08-29T12:34:56.000000Z","id":"01234567-89ab-cdef-0123-456789abcdef"}"#.as_slice(),
        br#"{"v":2,"field":"created_at","direction":"ascending","key":"2026-08-29T12:34:56.000000Z","id":"01234567-89ab-cdef-0123-456789abcdef"}"#.as_slice(),
    ];
    for payload in payloads {
        let encoded = URL_SAFE_NO_PAD.encode(payload);
        let error = decode_cursor(
            &encoded,
            "created_at",
            CursorDirection::Ascending,
            ColumnType::Timestamptz,
        )
        .unwrap_err();
        assert_eq!(error.error_type(), CursorErrorType::InvalidInput);
    }
}

#[test]
fn noncanonical_timestamps_and_numeric_spellings_refuse() {
    for (written, canonical, column) in [
        (
            "2026-08-29T12:34:56Z",
            "2026-08-29T12:34:56.000000Z",
            ColumnType::Timestamptz,
        ),
        (
            "2026-08-29T14:34:56.000000+02:00",
            "2026-08-29T12:34:56.000000Z",
            ColumnType::Timestamptz,
        ),
        (
            "2024-03-01T00:30:00+02:00",
            "2024-02-29T22:30:00.000000Z",
            ColumnType::Timestamptz,
        ),
        (
            "2026-08-29T12:34:56.1234567Z",
            "2026-08-29T12:34:56.123456Z",
            ColumnType::Timestamptz,
        ),
        ("01.0", "1.0", ColumnType::Numeric),
        ("1.", "1", ColumnType::Numeric),
        (".50", "0.50", ColumnType::Numeric),
        ("-00.00", "-0.00", ColumnType::Numeric),
        ("12.3400", "12.3400", ColumnType::Numeric),
        ("-0", "-0", ColumnType::Numeric),
    ] {
        let key = if column == ColumnType::Numeric {
            CursorValue::Numeric(written.into())
        } else {
            CursorValue::Timestamptz(written.into())
        };
        let encoded =
            encode_cursor(&CursorV1::new("value", CursorDirection::Ascending, key, ID)).unwrap();
        let expected = format!(
            r#"{{"direction":"ascending","field":"value","id":"{ID}","key":"{canonical}","v":1}}"#
        );
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&encoded).unwrap(),
            expected.as_bytes()
        );
        let decoded = decode_cursor(&encoded, "value", CursorDirection::Ascending, column).unwrap();
        assert_eq!(encode_cursor(&decoded).unwrap(), encoded);
        if written != canonical {
            let noncanonical = format!(
                r#"{{"direction":"ascending","field":"value","id":"{ID}","key":"{written}","v":1}}"#
            );
            assert!(
                decode_cursor(
                    &URL_SAFE_NO_PAD.encode(noncanonical),
                    "value",
                    CursorDirection::Ascending,
                    column
                )
                .is_err()
            );
        }
    }
    for numeric in ["1e2", "+1.00", " 1", "1 ", "NaN", "1.2.3"] {
        let error = encode_cursor(&CursorV1::new(
            "value",
            CursorDirection::Ascending,
            CursorValue::Numeric(numeric.into()),
            ID,
        ))
        .unwrap_err();
        assert_eq!(error.error_type(), CursorErrorType::InvalidInput);
    }
    for timestamp in [
        "2026-02-29T00:00:00Z",
        "2024-04-31T00:00:00Z",
        "2026-08-29T12:34:56+24:00",
        "1999-12-31T23:59:59.5-00:00:30",
    ] {
        let error = encode_cursor(&CursorV1::new(
            "value",
            CursorDirection::Ascending,
            CursorValue::Timestamptz(timestamp.into()),
            ID,
        ))
        .unwrap_err();
        assert_eq!(error.error_type(), CursorErrorType::InvalidInput);
    }
}

#[test]
fn captured_cursor_bytes_before_scalar_consolidation() {
    for (key, recorded) in [
        (
            CursorValue::Numeric("-0.00".into()),
            "eyJkaXJlY3Rpb24iOiJhc2NlbmRpbmciLCJmaWVsZCI6InZhbHVlIiwiaWQiOiIwMTIzNDU2Ny04OWFiLWNkZWYtMDEyMy00NTY3ODlhYmNkZWYiLCJrZXkiOiItMC4wMCIsInYiOjF9",
        ),
        (
            CursorValue::Numeric("12.3400".into()),
            "eyJkaXJlY3Rpb24iOiJhc2NlbmRpbmciLCJmaWVsZCI6InZhbHVlIiwiaWQiOiIwMTIzNDU2Ny04OWFiLWNkZWYtMDEyMy00NTY3ODlhYmNkZWYiLCJrZXkiOiIxMi4zNDAwIiwidiI6MX0",
        ),
        (
            CursorValue::Numeric("-1.50".into()),
            "eyJkaXJlY3Rpb24iOiJhc2NlbmRpbmciLCJmaWVsZCI6InZhbHVlIiwiaWQiOiIwMTIzNDU2Ny04OWFiLWNkZWYtMDEyMy00NTY3ODlhYmNkZWYiLCJrZXkiOiItMS41MCIsInYiOjF9",
        ),
        (
            CursorValue::Timestamptz("2026-08-29T12:34:56.000000Z".into()),
            "eyJkaXJlY3Rpb24iOiJhc2NlbmRpbmciLCJmaWVsZCI6InZhbHVlIiwiaWQiOiIwMTIzNDU2Ny04OWFiLWNkZWYtMDEyMy00NTY3ODlhYmNkZWYiLCJrZXkiOiIyMDI2LTA4LTI5VDEyOjM0OjU2LjAwMDAwMFoiLCJ2IjoxfQ",
        ),
        (
            CursorValue::Timestamptz("2024-02-29T23:59:59.123456Z".into()),
            "eyJkaXJlY3Rpb24iOiJhc2NlbmRpbmciLCJmaWVsZCI6InZhbHVlIiwiaWQiOiIwMTIzNDU2Ny04OWFiLWNkZWYtMDEyMy00NTY3ODlhYmNkZWYiLCJrZXkiOiIyMDI0LTAyLTI5VDIzOjU5OjU5LjEyMzQ1NloiLCJ2IjoxfQ",
        ),
        (
            CursorValue::Text("open".into()),
            "eyJkaXJlY3Rpb24iOiJhc2NlbmRpbmciLCJmaWVsZCI6InZhbHVlIiwiaWQiOiIwMTIzNDU2Ny04OWFiLWNkZWYtMDEyMy00NTY3ODlhYmNkZWYiLCJrZXkiOiJvcGVuIiwidiI6MX0",
        ),
        (
            CursorValue::Int64(7),
            "eyJkaXJlY3Rpb24iOiJhc2NlbmRpbmciLCJmaWVsZCI6InZhbHVlIiwiaWQiOiIwMTIzNDU2Ny04OWFiLWNkZWYtMDEyMy00NTY3ODlhYmNkZWYiLCJrZXkiOjcsInYiOjF9",
        ),
    ] {
        let encoded =
            encode_cursor(&CursorV1::new("value", CursorDirection::Ascending, key, ID)).unwrap();
        assert_eq!(encoded, recorded);
    }
}
