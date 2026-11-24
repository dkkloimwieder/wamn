use wamn_authority::connection_authority::{TlsPolicy, parse_http_connection_authority};

#[test]
fn definition_rejects_userinfo_fragments_incoherent_tls() {
    let cases = [
        ("https://user@erp.example/api", TlsPolicy::VerifyAuthority),
        ("https://erp.example/api#frag", TlsPolicy::VerifyAuthority),
        (
            "https://erp.example/api/../private",
            TlsPolicy::VerifyAuthority,
        ),
        (
            "https://erp.example/api/%2e%2e/private",
            TlsPolicy::VerifyAuthority,
        ),
        ("https://%65rp.example/api", TlsPolicy::VerifyAuthority),
        ("https://erp.example/api", TlsPolicy::Disabled),
        ("http://erp.example/api", TlsPolicy::VerifyAuthority),
    ];
    for (base, tls) in cases {
        assert!(
            parse_http_connection_authority(base, tls).is_err(),
            "accepted invalid definition {base:?}"
        );
    }
}
