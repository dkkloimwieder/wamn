use base64::Engine as _;

use super::oauth2_tests::Fixture;
use super::*;

fn request() -> Request {
    Request {
        requirement: "handle".into(),
        path_and_query: "/api".into(),
        body: Some(b"guest-body".to_vec()),
        ..effect_request(Vec::new())
    }
}

fn basic(password: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("fixture:{password}"))
    )
}

#[tokio::test]
#[ignore = "requires: PostgreSQL 18"]
async fn a_401_after_a_write_retries_once_with_the_new_basic_credential() {
    let _lock = wamn_test_postgres::lock();
    for second in [200, 401] {
        let mut fixture = Fixture::start(Vec::new(), vec![401, second]).await;
        let plugin = fixture.basic_plugin().await;
        let (resume, arrival) = fixture.pause_next_response();
        let request = request();
        let (response, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(plugin.send("instance", &request), async {
                arrival.await.unwrap();
                fixture.write_basic().await;
                resume.send(()).unwrap();
            })
        })
        .await
        .expect("the write finishes before the first 401 reaches the host");
        let response = response.unwrap();
        assert_eq!(response.status, second);
        assert_eq!(response.body, format!("api-{second}").as_bytes());
        assert_eq!(fixture.counts(), (0, 2));
        assert_eq!(fixture.authorizations(), [basic("old"), basic("new")]);
    }
}

#[tokio::test]
#[ignore = "requires: PostgreSQL 18"]
async fn a_401_without_a_change_returns_the_first_response_after_one_send() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(Vec::new(), vec![401, 200]).await;
    let plugin = fixture.basic_plugin().await;
    let response = plugin.send("instance", &request()).await.unwrap();
    assert_eq!(response.status, 401);
    assert_eq!(response.body, b"api-401");
    assert_eq!(fixture.counts(), (0, 1));
    assert_eq!(fixture.authorizations(), [basic("old")]);
}
