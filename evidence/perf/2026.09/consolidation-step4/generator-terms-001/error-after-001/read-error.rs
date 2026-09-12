use std::error::Error as _;

fn main() {
    let error = wamn_schema_generator::GeneratedPackageMetadata::from_slice(b"null").unwrap_err();
    println!("{error}");
    println!("{}", error.source().unwrap());
}
