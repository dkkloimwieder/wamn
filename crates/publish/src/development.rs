use wamn_build::development_version;

#[cfg(test)]
mod tests {
    #[test]
    fn development_versions_require_a_content_digest() {
        let digest = wamn_digest::bytes_sha256(b"source");
        let version = super::development_version(&"1.0.0".parse().unwrap(), &digest).unwrap();
        assert!(wamn_oci::package_artifact::is_dev_tag(
            &version.to_string().parse().unwrap()
        ));
    }
}
