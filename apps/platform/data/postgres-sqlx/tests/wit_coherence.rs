#[test]
fn vendored_postgres_wit_matches_the_existing_guest_copy() {
    let driver = include_str!("../wit/deps/wamn-postgres/package.wit");
    let materializer =
        include_str!("../../../execution/materializer/wit/deps/wamn-postgres/package.wit");
    assert_eq!(driver, materializer);
}
