//! Parse JSON-valued HTTP query parameters.

use serde_json::{Map, Value};

/// Parse each parameter value as JSON. A repeated name keeps its last value.
pub fn decode_read_query(query: &str) -> Result<Map<String, Value>, serde_json::Error> {
    url::form_urlencoded::parse(query.as_bytes())
        .map(|(name, value)| serde_json::from_str(&value).map(|value| (name.into_owned(), value)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::decode_read_query;
    use serde_json::{Value, json};

    #[test]
    fn every_vector_decodes_to_its_request_element() {
        let vectors: Vec<Value> = serde_json::from_str(include_str!(
            "../../../../../crates/client/core/read-query-vectors.json"
        ))
        .expect("read-query vectors are JSON");
        for vector in vectors {
            let query = vector["query"].as_str().expect("a vector query is text");
            let decoded = Value::Object(decode_read_query(query).expect("a vector query parses"));
            assert_eq!(decoded, vector["item"]);
        }
    }

    #[test]
    fn read_parameters_accept_order_and_escaping_variants() {
        let expected = json!({"a": [1, 2], "b": "a b"});
        for query in ["b=%22a+b%22&a=%5b1%2c2%5d", "a=[1,2]&b=%22a%20b%22"] {
            assert_eq!(Value::Object(decode_read_query(query).unwrap()), expected);
        }
        assert!(decode_read_query("a=invalid").is_err());
    }
}
