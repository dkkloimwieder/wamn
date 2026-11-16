#[path = "../src/read_query.rs"]
mod read_query;

use read_query::encode_read_query;
use serde_json::{Map, Value};

#[test]
fn every_vector_encodes_its_request_element() {
    let vectors: Vec<Value> = serde_json::from_str(include_str!("../read-query-vectors.json"))
        .expect("read-query vectors are JSON");
    for vector in vectors {
        let item = vector["item"]
            .as_object()
            .expect("a request element is an object");
        let encoded = encode_read_query(item);
        for query in [
            encoded.as_str(),
            vector["query"].as_str().expect("a vector query is text"),
        ] {
            let decoded: Map<String, Value> = url::form_urlencoded::parse(query.as_bytes())
                .map(|(name, value)| {
                    (
                        name.into_owned(),
                        serde_json::from_str(&value).expect("a query value is JSON"),
                    )
                })
                .collect();
            assert_eq!(Value::Object(decoded), vector["item"]);
        }
    }
}
