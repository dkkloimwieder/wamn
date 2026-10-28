//! Forward S3 requests to the owned MinIO and selectively refuse object deletion.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};

pub struct DeleteProxy {
    pub endpoint: String,
    pub refuse: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl DeleteProxy {
    pub async fn start(upstream: String) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let refuse = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&refuse);
        let client = reqwest::Client::builder().no_proxy().build()?;
        let task = tokio::spawn(async move {
            let mut requests = tokio::task::JoinSet::new();
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (upstream, flag, client) =
                    (upstream.clone(), Arc::clone(&flag), client.clone());
                requests.spawn(async move {
                    let service = hyper::service::service_fn(
                        move |request: hyper::Request<hyper::body::Incoming>| {
                            let (upstream, flag, client) =
                                (upstream.clone(), Arc::clone(&flag), client.clone());
                            async move {
                                let deletes = request.method() == hyper::Method::DELETE
                                    || (request.method() == hyper::Method::POST
                                        && request.uri().query().is_some_and(|query| {
                                            query.split('&').any(|part| {
                                                part == "delete" || part.starts_with("delete=")
                                            })
                                        }));
                                if deletes && flag.load(Ordering::SeqCst) {
                                    return anyhow::Ok(
                                        hyper::Response::builder().status(403).body(Full::new(
                                            Bytes::from_static(
                                                b"<Error><Code>AccessDenied</Code></Error>",
                                            ),
                                        ))?,
                                    );
                                }
                                let (parts, body) = request.into_parts();
                                let response = client
                                    .request(parts.method, format!("{upstream}{}", parts.uri))
                                    .headers(parts.headers)
                                    .body(body.collect().await?.to_bytes())
                                    .send()
                                    .await?;
                                let mut result =
                                    hyper::Response::builder().status(response.status());
                                *result.headers_mut().unwrap() = response.headers().clone();
                                Ok(result.body(Full::new(response.bytes().await?))?)
                            }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
                while requests.try_join_next().is_some() {}
            }
        });
        Ok(Self {
            endpoint,
            refuse,
            task,
        })
    }
}

impl Drop for DeleteProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}
