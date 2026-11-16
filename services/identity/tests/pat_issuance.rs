//! The native identity process issues PATs only to verified TLS operators.

use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, date_time_ymd,
};
use wamn_identity::tls_config_with_operator_ca;

// Upstream errors and response bodies can contain secrets. Never format them.
trait ExpectRedacted<T> {
    fn expect_redacted(self, context: &'static str) -> T;
}

impl<T, E> ExpectRedacted<T> for Result<T, E> {
    fn expect_redacted(self, context: &'static str) -> T {
        self.ok().expect(context)
    }
}

struct Material {
    certificate: Vec<u8>,
    key: Vec<u8>,
}

#[test]
fn operator_ca_configuration_refuses_missing_or_malformed_roots() {
    let (issuer, _) = authority();
    let server = certificate(&issuer, false, false);
    for roots in [b"".as_slice(), b"not a certificate".as_slice()] {
        let error = tls_config_with_operator_ca(&server.certificate, &server.key, roots)
            .expect_err("invalid operator CA refuses listener startup");
        let detail = format!("{error:?} {error}");
        for line in std::str::from_utf8(&server.key).unwrap().lines() {
            assert!(!detail.contains(line));
        }
    }
}

fn authority() -> (Issuer<'static, KeyPair>, Vec<u8>) {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect_redacted("CA parameters");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let key = KeyPair::generate().expect_redacted("CA key");
    let certificate = params.self_signed(&key).expect_redacted("CA certificate");
    (Issuer::new(params, key), certificate.pem().into_bytes())
}

fn certificate(issuer: &Issuer<'_, KeyPair>, operator: bool, expired: bool) -> Material {
    let mut params =
        CertificateParams::new(vec!["127.0.0.1".to_owned()]).expect_redacted("TLS parameters");
    params.extended_key_usages = vec![if operator {
        ExtendedKeyUsagePurpose::ClientAuth
    } else {
        ExtendedKeyUsagePurpose::ServerAuth
    }];
    if expired {
        params.not_before = date_time_ymd(2000, 1, 1);
        params.not_after = date_time_ymd(2001, 1, 1);
    }
    let key = KeyPair::generate().expect_redacted("TLS key");
    let certificate = params
        .signed_by(&key, issuer)
        .expect_redacted("TLS certificate");
    Material {
        certificate: certificate.pem().into_bytes(),
        key: key.serialize_pem().into_bytes(),
    }
}
