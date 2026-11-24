//! HTTP boundary fixtures for this crate's unit tests.
use wamn_connection::bound_http::{Header, Request};
pub fn interface_address() -> std::net::IpAddr {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").expect("address selection socket");
    socket
        .connect("192.0.2.1:9")
        .expect("select a private interface");
    socket.local_addr().expect("interface address").ip()
}

pub fn effect_request(headers: Vec<Header>) -> Request {
    Request {
        method: "POST".to_string(),
        path_and_query: "/notify".to_string(),
        headers,
        body: None,
        idempotency_key: None,
    }
}

pub(crate) const CLIENT_ID: &str = "private client:id+";
pub(crate) const CLIENT_SECRET: &str = "private/secret&+";

#[derive(Debug)]
pub struct Observed {
    pub path: String,
    pub method: String,
    pub headers: hyper::HeaderMap,
    pub body: Vec<u8>,
}
