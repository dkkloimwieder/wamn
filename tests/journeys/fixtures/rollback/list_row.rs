|source| {
    let mut row = operation::row!(source, contract::ListRow);
    "priority".clone_into(&mut row.code);
    row
}
