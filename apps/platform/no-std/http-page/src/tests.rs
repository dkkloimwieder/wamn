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
        let future = collect_pages(config, Vec::new(), |request| {
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
