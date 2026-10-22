use super::*;
use alloc::vec;
use core::{
    future::Future as _,
    pin::pin,
    task::{Context, Poll, Waker},
};
use serde_json::{Value, json};

fn run(config: &Value, pages: Vec<Response>) -> (Result<Vec<Value>, NodeError>, Vec<String>) {
    let mut paths = Vec::new();
    let mut pages = pages.into_iter();
    let result = {
        let future = collect_pages(config, &Value::Null, Vec::new(), |request| {
            assert_eq!(request.method, "GET");
            assert!(request.body.is_none());
            paths.push(request.path_and_query);
            core::future::ready(Ok(pages.next().expect("unexpected page request")))
        });
        let waker = Waker::noop();
        match pin!(future).poll(&mut Context::from_waker(waker)) {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("pure send closure must complete"),
        }
    };
    assert!(pages.next().is_none(), "unused response");
    (result, paths)
}
fn config(page: &Value) -> Value {
    json!({"requirement":"upstream", "path-and-query":"/items?limit=2", "headers":{}, "page":page,"items":"/items"})
}
fn response(body: &Value) -> Response {
    Response {
        status: 200,
        headers: Vec::new(),
        body: body.to_string().into_bytes(),
    }
}
#[test]
fn offset_collects_pages_and_stops_on_short_last_page() {
    let (result, paths) = run(
        &config(&json!({"style":"offset","limit":"limit","offset":"offset"})),
        vec![
            response(&json!({"items":[1,2]})),
            response(&json!({"items":[3,4]})),
            response(&json!({"items":[5]})),
        ],
    );
    assert_eq!(
        result.unwrap(),
        vec![json!(1), json!(2), json!(3), json!(4), json!(5)]
    );
    assert_eq!(
        paths,
        [
            "/items?limit=2",
            "/items?limit=2&offset=2",
            "/items?limit=2&offset=4"
        ]
    );
}
#[test]
fn cursor_encodes_and_stops_on_absent_null_or_empty() {
    for last in [
        json!({"items":[3]}),
        json!({"items":[3],"next":null}),
        json!({"items":[3],"next":""}),
    ] {
        let (result, paths) = run(
            &config(&json!({"style":"cursor","pointer":"/next","param":"after"})),
            vec![
                response(&json!({"items":[1,2],"next":"a+b /"})),
                response(&last),
            ],
        );
        assert_eq!(result.unwrap(), vec![json!(1), json!(2), json!(3)]);
        assert_eq!(paths[1], "/items?limit=2&after=a%2Bb%20%2F");
    }
}
#[test]
fn relative_link_is_sent_unchanged() {
    let mut first = response(&json!({"items":[1]}));
    first.headers.push(Header {
        name: "Link".into(),
        value: br#"</items?p=2>; rel="next", </items?p=1>; rel="prev"#.to_vec(),
    });
    let (result, paths) = run(
        &config(&json!({"style":"link-header"})),
        vec![first, response(&json!({"items":[2]}))],
    );
    assert_eq!(result.unwrap(), vec![json!(1), json!(2)]);
    assert_eq!(paths[1], "/items?p=2");
}
#[test]
fn absolute_links_are_terminal() {
    for target in [
        "http://http-fixture:8080/items?p=2",
        "https://foreign.invalid/items?p=2",
        "//foreign.invalid/items",
    ] {
        let mut page = response(&json!({"items":[1]}));
        page.headers.push(Header {
            name: "link".into(),
            value: format!("<{target}>; rel=\"next\"").into_bytes(),
        });
        assert!(matches!(
            run(&config(&json!({"style":"link-header"})), vec![page]).0,
            Err(NodeError::Terminal(_))
        ));
    }
}
#[test]
fn page_two_rate_limit_discards_partial_result() {
    let mut limited = response(&json!({}));
    limited.status = 429;
    limited.headers.push(Header {
        name: "Retry-After".into(),
        value: b"3".to_vec(),
    });
    let (result, paths) = run(
        &config(&json!({"style":"offset","limit":"limit","offset":"offset"})),
        vec![response(&json!({"items":[1,2]})), limited],
    );
    let Err(NodeError::RateLimited(detail)) = result else {
        panic!("expected rate limit")
    };
    assert_eq!(detail.retry_after_ms, Some(3000));
    assert_eq!(paths.len(), 2);
}
#[test]
fn client_errors_are_terminal_and_server_errors_retryable() {
    for status in [400, 401, 408, 499, 500, 503, 599] {
        let mut page = response(&json!({}));
        page.status = status;
        let result = run(&config(&json!({"style":"link-header"})), vec![page]).0;
        if status < 500 {
            assert!(matches!(result, Err(NodeError::Terminal(_))));
        } else {
            assert!(matches!(result, Err(NodeError::Retryable(_))));
        }
    }
}
#[test]
fn invalid_items_and_missing_limit_are_terminal() {
    for body in [json!({}), json!({"items":null}), json!({"items":{}})] {
        assert!(matches!(
            run(
                &config(&json!({"style":"link-header"})),
                vec![response(&body)]
            )
            .0,
            Err(NodeError::Terminal(_))
        ));
    }
    let mut cfg = config(&json!({"style":"offset","limit":"size","offset":"offset"}));
    assert!(matches!(run(&cfg, vec![]).0, Err(NodeError::Terminal(_))));
    cfg["path-and-query"] = json!("/items?size=0");
    assert!(matches!(run(&cfg, vec![]).0, Err(NodeError::Terminal(_))));
}

#[test]
fn carry_preserves_the_selected_input_and_empty_pages() {
    let input = json!({"due": "2026-10-21T12:00:00Z"});
    let config = json!({"carry-input-pointer": "/due"});
    let carry = super::carried_input(&config, &input).unwrap();
    assert_eq!(
        super::page_output(vec![json!({"id": "1"})], carry.clone()),
        json!({"items": [{"id": "1"}], "carry": input["due"]})
    );
    assert_eq!(
        super::page_output(vec![], carry),
        json!({"items": [], "carry": input["due"]})
    );
    assert_eq!(
        super::page_output(
            vec![json!(1)],
            super::carried_input(&json!({}), &input).unwrap()
        ),
        json!([1])
    );
    assert!(matches!(
        super::carried_input(&json!({"carry-input-pointer": "/absent"}), &input),
        Err(NodeError::Terminal(_))
    ));
    assert_eq!(
        super::carried_input(&config, &json!({"due": null})).unwrap(),
        Some(serde_json::Value::Null)
    );
}

fn run_with_input(
    config: &Value,
    input: &Value,
    pages: Vec<Response>,
) -> (Result<Vec<Value>, NodeError>, Vec<Request>) {
    let mut requests = Vec::new();
    let mut pages = pages.into_iter();
    let result = {
        let future = collect_pages(
            config,
            input,
            configured_headers(config).unwrap(),
            |request| {
                requests.push(request);
                core::future::ready(Ok(pages.next().expect("unexpected page request")))
            },
        );
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("pure send closure must complete"),
        }
    };
    assert!(pages.next().is_none(), "unused response");
    (result, requests)
}

#[test]
fn post_carries_json_and_query_parameters_until_has_more_is_false() {
    for initial_offset in [None, Some(4)] {
        let mut cfg = config(&json!({
            "style":"offset", "limit":"limit", "offset":"offset", "has-more":"/hasMore"
        }));
        cfg["method"] = json!("POST");
        cfg["body-input-pointer"] = json!("/query");
        cfg["carry-input-pointer"] = json!("/due");
        cfg["headers"] = json!({"Prefer":"transient", "Content-Type":"text/plain"});
        cfg["path-and-query"] = json!(match initial_offset {
            Some(offset) => format!("/services/rest/query/v1/suiteql?limit=2&offset={offset}"),
            None => "/services/rest/query/v1/suiteql?limit=2".into(),
        });
        let input = json!({"query":{"q":"SELECT id FROM customer ORDER BY id"}, "due":"2026-10-22T12:00:00Z"});
        let (result, requests) = run_with_input(
            &cfg,
            &input,
            vec![
                response(&json!({"items":[1,2], "hasMore":true})),
                response(&json!({"items":[3,4], "hasMore":false})),
            ],
        );
        assert_eq!(
            page_output(result.unwrap(), carried_input(&cfg, &input).unwrap()),
            json!({"items":[1,2,3,4], "carry":input["due"]})
        );
        assert_eq!(requests.len(), 2);
        for (index, request) in requests.iter().enumerate() {
            assert_eq!(request.method, "POST");
            assert_eq!(
                request.path_and_query,
                format!(
                    "/services/rest/query/v1/suiteql?limit=2&offset={}",
                    initial_offset.unwrap_or(0) + index * 2
                )
            );
            assert_eq!(
                serde_json::from_slice::<Value>(request.body.as_ref().unwrap()).unwrap(),
                input["query"]
            );
            let content_types: Vec<_> = request
                .headers
                .iter()
                .filter(|header| header.name.eq_ignore_ascii_case("content-type"))
                .map(|header| header.value.as_slice())
                .collect();
            assert_eq!(content_types, [b"application/json".as_slice()]);
            assert!(
                request
                    .headers
                    .iter()
                    .any(|header| header.name == "prefer" && header.value == b"transient")
            );
        }
    }
}

#[test]
fn has_more_true_continues_after_a_short_page() {
    let (result, paths) = run(
        &config(
            &json!({"style":"offset", "limit":"limit", "offset":"offset", "has-more":"/hasMore"}),
        ),
        vec![
            response(&json!({"items":[1], "hasMore":true})),
            response(&json!({"items":[2], "hasMore":false})),
        ],
    );
    assert_eq!(result.unwrap(), vec![json!(1), json!(2)]);
    assert_eq!(paths, ["/items?limit=2", "/items?limit=2&offset=2"]);
}

#[test]
fn missing_or_nonboolean_has_more_is_terminal_even_on_a_short_page() {
    for last in [
        json!({"items":[]}),
        json!({"items":[], "hasMore":null}),
        json!({"items":[], "hasMore":"false"}),
        json!({"items":[], "hasMore":0}),
    ] {
        let (result, paths) = run(
            &config(
                &json!({"style":"offset", "limit":"limit", "offset":"offset", "has-more":"/hasMore"}),
            ),
            vec![
                response(&json!({"items":[1,2], "hasMore":true})),
                response(&last),
            ],
        );
        let Err(NodeError::Terminal(detail)) = result else {
            panic!("expected terminal error")
        };
        assert_eq!(detail.code.as_deref(), Some("invalid-page"));
        assert_eq!(detail.message, "has-more pointer must select a boolean");
        assert_eq!(paths.len(), 2);
    }
}

#[test]
fn missing_body_target_is_terminal_before_sending() {
    let mut cfg = config(&json!({"style":"offset", "limit":"limit", "offset":"offset"}));
    cfg["method"] = json!("POST");
    cfg["body-input-pointer"] = json!("/absent");
    let (result, requests) = run_with_input(&cfg, &json!({"query":{}}), vec![]);
    let Err(NodeError::Terminal(detail)) = result else {
        panic!("expected terminal error")
    };
    assert_eq!(detail.code.as_deref(), Some("invalid-input-pointer"));
    assert_eq!(
        detail.message,
        "body-input-pointer has no target in the input"
    );
    assert!(requests.is_empty());
}

#[test]
fn body_pointer_accepts_root_and_null_values() {
    for (pointer, input, expected) in [
        (
            "",
            json!({"q":"SELECT id FROM customer ORDER BY id"}),
            json!({"q":"SELECT id FROM customer ORDER BY id"}),
        ),
        ("/query", json!({"query":null}), Value::Null),
    ] {
        let mut cfg = config(&json!({"style":"link-header"}));
        cfg["method"] = json!("POST");
        cfg["body-input-pointer"] = json!(pointer);
        let (result, requests) = run_with_input(&cfg, &input, vec![response(&json!({"items":[]}))]);
        assert!(result.unwrap().is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap(),
            expected
        );
    }
}

#[test]
fn nonstring_paging_parameters_are_invalid_configuration() {
    let cfg = config(&json!({"style":"offset", "limit":"limit", "offset":"offset"}));
    for pointer in ["/method", "/body-input-pointer", "/page/has-more"] {
        let mut cfg = cfg.clone();
        if pointer == "/page/has-more" {
            cfg["page"]["has-more"] = json!(false);
        } else {
            cfg[pointer.trim_start_matches('/')] = json!(false);
        }
        let (result, requests) = run_with_input(&cfg, &Value::Null, vec![]);
        let Err(NodeError::Terminal(detail)) = result else {
            panic!("expected terminal error")
        };
        assert_eq!(detail.code.as_deref(), Some("invalid-config"));
        assert!(requests.is_empty());
    }
}
