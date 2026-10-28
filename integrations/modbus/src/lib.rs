//! Read holding registers through the host-owned Modbus TCP binding.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;
use alloc::vec::Vec;

#[cfg(target_arch = "wasm32")]
#[path = "../../../apps/platform/no-std/guest_runtime.rs"]
mod guest_runtime;

#[cfg(target_arch = "wasm32")]
mod component {
    use alloc::{format, string::String, vec::Vec};
    wit_bindgen::generate!({
        path: ["../../crates/platform/runtime/wit/deps/wamn-connector", "wit"],
        world: "wamn:modbus/modbus",
        generate_all,
        std_feature,
    });

    use exports::wamn::connector::reader::{ConnectorError, Guest};
    use wasi::sockets::network::{
        ErrorCode, IpAddressFamily, IpSocketAddress, Ipv4SocketAddress, Ipv6SocketAddress,
    };

    struct Modbus;
    export!(Modbus);

    fn transport(error: impl core::fmt::Debug) -> ConnectorError {
        ConnectorError::Transport(format!("{error:?}"))
    }

    fn socket_error(error: ErrorCode) -> ConnectorError {
        match error {
            ErrorCode::AccessDenied => ConnectorError::AuthorityDenied,
            ErrorCode::Timeout => ConnectorError::Timeout,
            error => transport(error),
        }
    }

    impl Guest for Modbus {
        fn get(_: String, request: Vec<u8>) -> Result<Vec<u8>, ConnectorError> {
            let frame = super::request_frame(&request).map_err(transport)?;
            let authority = wamn::connector::bound::authority();
            // Modbus connections use an explicit IP literal and TCP port.
            let address: core::net::SocketAddr = authority.parse().map_err(transport)?;
            let (family, address) = match address {
                core::net::SocketAddr::V4(address) => {
                    let [a, b, c, d] = address.ip().octets();
                    (
                        IpAddressFamily::Ipv4,
                        IpSocketAddress::Ipv4(Ipv4SocketAddress {
                            port: address.port(),
                            address: (a, b, c, d),
                        }),
                    )
                }
                core::net::SocketAddr::V6(address) => {
                    let [a, b, c, d, e, f, g, h] = address.ip().segments();
                    (
                        IpAddressFamily::Ipv6,
                        IpSocketAddress::Ipv6(Ipv6SocketAddress {
                            port: address.port(),
                            address: (a, b, c, d, e, f, g, h),
                            flow_info: 0,
                            scope_id: 0,
                        }),
                    )
                }
            };
            let network = wasi::sockets::instance_network::instance_network();
            let socket = wasi::sockets::tcp_create_socket::create_tcp_socket(family)
                .map_err(socket_error)?;
            socket
                .start_connect(&network, address)
                .map_err(socket_error)?;
            let (input, output) = loop {
                match socket.finish_connect() {
                    Ok(streams) => break streams,
                    Err(ErrorCode::WouldBlock) => socket.subscribe().block(),
                    Err(error) => return Err(socket_error(error)),
                }
            };
            output.blocking_write_and_flush(&frame).map_err(transport)?;
            let mut response = Vec::new();
            // A normal response has nine header bytes and two bytes per register.
            let size = 9 + usize::from(u16::from_be_bytes([request[3], request[4]])) * 2;
            while response.len() < size {
                let bytes = input
                    .blocking_read((size - response.len()) as u64)
                    .map_err(transport)?;
                if bytes.is_empty() {
                    continue;
                }
                response.extend(bytes);
                if response.len() >= 9 && response[7] & 0x80 != 0 {
                    return Err(transport("Modbus exception"));
                }
            }
            super::response_registers(&frame, &response).map_err(transport)
        }
    }
}

/// Encode unit, start and count as five bytes. Both integers use network order.
pub fn request_frame(request: &[u8]) -> Result<[u8; 12], &'static str> {
    let [unit, start_hi, start_lo, count_hi, count_lo] = *request else {
        return Err("request needs a unit, start address and count");
    };
    let start = u16::from_be_bytes([start_hi, start_lo]);
    let count = u16::from_be_bytes([count_hi, count_lo]);
    if unit == 0
        || unit > 247
        || count == 0
        || count > 125
        || u32::from(start) + u32::from(count) > 65536
    {
        return Err("invalid Modbus register range");
    }
    // One request per TCP connection uses transaction 1 and protocol 0.
    Ok([
        0, 1, 0, 0, 0, 6, unit, 3, start_hi, start_lo, count_hi, count_lo,
    ])
}

/// Return the raw big-endian register bytes after the response matches its request.
pub fn response_registers(request: &[u8; 12], response: &[u8]) -> Result<Vec<u8>, &'static str> {
    let count = usize::from(u16::from_be_bytes([request[10], request[11]]));
    let bytes = count * 2;
    if response.len() != 9 + bytes
        || response[..4] != request[..4]
        || usize::from(u16::from_be_bytes([response[4], response[5]])) != bytes + 3
        || response[6] != request[6]
        || response[7] != 3
        || usize::from(response[8]) != bytes
    {
        return Err("Modbus response does not match the request");
    }
    Ok(response[9..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_ranges_and_mismatched_responses_refuse() {
        for request in [
            vec![],
            vec![0, 0, 0, 0, 1],
            vec![1, 0, 0, 0, 0],
            vec![1, 0, 0, 0, 126],
            vec![1, 255, 255, 0, 2],
        ] {
            assert!(request_frame(&request).is_err());
        }
        let request = request_frame(&[1, 0, 7, 0, 1]).unwrap();
        let response = vec![0, 1, 0, 0, 0, 5, 1, 3, 2, 0x12, 0x34];
        assert_eq!(
            response_registers(&request, &response).unwrap(),
            [0x12, 0x34]
        );
        for index in [0, 2, 4, 5, 6, 7, 8] {
            let mut wrong = response.clone();
            wrong[index] ^= 1;
            assert!(response_registers(&request, &wrong).is_err());
        }
        assert!(response_registers(&request, &response[..8]).is_err());
    }
}
