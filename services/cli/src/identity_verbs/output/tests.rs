use super::write_secret_json;
use serde_json::{Value, json};
use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn secret_writes_replace_links_without_following_or_sharing_them() {
    let work = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    for file_type in ["file", "symlink", "hardlink"] {
        let source = work.path().join(format!("{file_type}-source.json"));
        let output = work.path().join(format!("{file_type}-output.json"));
        let original = json!({"stringData": {"token": "original-secret"}});
        std::fs::write(&source, serde_json::to_vec(&original).unwrap()).unwrap();
        match file_type {
            "file" => {
                std::fs::write(&output, b"old credential material").unwrap();
                std::fs::set_permissions(&output, Permissions::from_mode(0o644)).unwrap();
            }
            "symlink" => std::os::unix::fs::symlink(&source, &output).unwrap(),
            "hardlink" => std::fs::hard_link(&source, &output).unwrap(),
            _ => unreachable!(),
        }
        let replacement = json!({"stringData": {"token": "replacement-secret"}});
        write_secret_json(&output, &replacement).unwrap();
        assert!(
            !std::fs::symlink_metadata(&output)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let stored: Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(
            stored["stringData"]["token"],
            replacement["stringData"]["token"]
        );
        let retained: Value = serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
        assert_eq!(
            retained["stringData"]["token"],
            original["stringData"]["token"]
        );
    }
}
