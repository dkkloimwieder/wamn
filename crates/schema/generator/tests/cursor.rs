use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use wamn_schema_generator::{
    CursorDirection, CursorErrorKind, CursorV1, CursorValue, decode_cursor, encode_cursor,
};
use wamn_schema_introspection::ir::ColumnType;

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
        encoded,
        "eyJ2IjoxLCJmaWVsZCI6ImNyZWF0ZWRfYXQiLCJkaXJlY3Rpb24iOiJhc2NlbmRpbmciLCJrZXkiOiIyMDI2LTA4LTI5VDEyOjM0OjU2LjAwMDAwMFoiLCJpZCI6IjAxMjM0NTY3LTg5YWItY2RlZi0wMTIzLTQ1Njc4OWFiY2RlZiJ9"
    );
    assert_eq!(
        URL_SAFE_NO_PAD.decode(&encoded).unwrap(),
        br#"{"v":1,"field":"created_at","direction":"ascending","key":"2026-08-29T12:34:56.000000Z","id":"01234567-89ab-cdef-0123-456789abcdef"}"#
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
        encoded,
        "eyJ2IjoxLCJmaWVsZCI6ImFtb3VudCIsImRpcmVjdGlvbiI6ImRlc2NlbmRpbmciLCJrZXkiOiIxMi4zNDAwIiwiaWQiOiIwMTIzNDU2Ny04OWFiLWNkZWYtMDEyMy00NTY3ODlhYmNkZWYifQ"
    );
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
        assert_eq!(error.kind(), CursorErrorKind::InvalidInput);
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
        assert_eq!(error.kind(), CursorErrorKind::InvalidInput);
    }
}

#[test]
fn noncanonical_timestamps_and_numeric_spellings_refuse() {
    for timestamp in [
        "2026-08-29T12:34:56Z",
        "2026-08-29T12:34:56.000000+00:00",
        "2026-08-29T12:34:56.00000Z",
    ] {
        let error = encode_cursor(&CursorV1::new(
            "created_at",
            CursorDirection::Ascending,
            CursorValue::Timestamptz(timestamp.into()),
            ID,
        ))
        .unwrap_err();
        assert_eq!(error.kind(), CursorErrorKind::InvalidInput);
    }
    for numeric in ["01.0", "1e2", "+1.00", "1."] {
        let error = encode_cursor(&CursorV1::new(
            "amount",
            CursorDirection::Ascending,
            CursorValue::Numeric(numeric.into()),
            ID,
        ))
        .unwrap_err();
        assert_eq!(error.kind(), CursorErrorKind::InvalidInput);
    }
}
