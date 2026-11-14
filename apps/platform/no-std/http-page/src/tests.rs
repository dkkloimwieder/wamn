use super::*;
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
    (result.map(|pages| pages.items), paths)
}
fn config(page: &Value) -> Value {
    json!({"requirement":"upstream", "path-and-query":"/items?limit=2", "headers":{}, "page":page,"items":"/items"})
}
fn response(body: &Value) -> Response {
    Response {
        external_system: None,
        status: 200,
        headers: Vec::new(),
        body: body.to_string().into_bytes(),
    }
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
fn missing_carried_input_is_terminal() {
    assert!(matches!(
        carried_input(&json!({"carry-input-pointer": "/absent"}), &json!({})),
        Err(NodeError::Terminal(_))
    ));
}

fn run_with_input(
    config: &Value,
    input: &Value,
    pages: Vec<Response>,
) -> (Result<Pages, NodeError>, Vec<Request>) {
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
        let Err(NodeError::Terminal(_)) = result else {
            panic!("expected terminal error")
        };
        assert_eq!(paths.len(), 2);
    }
}

#[test]
fn missing_body_target_is_terminal_before_sending() {
    let mut cfg = config(&json!({"style":"offset", "limit":"limit", "offset":"offset"}));
    cfg["method"] = json!("POST");
    cfg["body-input-pointer"] = json!("/absent");
    let (result, requests) = run_with_input(&cfg, &json!({"query":{}}), vec![]);
    let Err(NodeError::Terminal(_)) = result else {
        panic!("expected terminal error")
    };
    assert!(requests.is_empty());
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
        let Err(NodeError::Terminal(_)) = result else {
            panic!("expected terminal error")
        };
        assert!(requests.is_empty());
    }
}

#[test]
fn a_changed_source_between_pages_refuses_the_batch() {
    for next in [Some("00000000-0000-4000-8000-00000000e002"), None] {
        let mut first = response(&json!({"items":[1,2]}));
        first.external_system = Some("00000000-0000-4000-8000-00000000e001".into());
        let mut second = response(&json!({"items":[3]}));
        second.external_system = next.map(String::from);
        let (result, _) = run_with_input(
            &config(&json!({"style":"offset","limit":"limit","offset":"offset"})),
            &Value::Null,
            vec![first, second],
        );
        assert!(matches!(result, Err(NodeError::Terminal(_))));
    }
}
