//! Published image coordinates required by platform installation.

use serde_json::Value;

fn pinned(image: &str, reference: &str) -> bool {
    image
        .strip_prefix(&format!("{reference}@sha256:"))
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

pub(super) fn images_match(values: &Value, registry: &str, version: &str) -> bool {
    let host = &values["runtime-operator"]["runtime"]["image"];
    let host_registry = host["registry"].as_str().unwrap_or_default();
    let repository = host["repository"].as_str().unwrap_or_default();
    let tag = host["tag"].as_str().unwrap_or_default();
    let host = if host_registry.is_empty() {
        format!("{repository}:{tag}")
    } else {
        format!("{host_registry}/{repository}:{tag}")
    };
    pinned(&host, &format!("{registry}/wamn-host:{version}"))
        && ["runner", "gates", "identity"].iter().all(|name| {
            pinned(
                values["platform"][name]["image"]
                    .as_str()
                    .unwrap_or_default(),
                &format!("{registry}/wamn-{name}:{version}"),
            )
        })
        && pinned(
            values["platform"]["edge"]["image"]
                .as_str()
                .unwrap_or_default(),
            &format!("{registry}/wamn-edge-web:{version}"),
        )
        && ["http", "materializer"].iter().all(|name| {
            pinned(
                values["platform"]["roles"][name]["image"]
                    .as_str()
                    .unwrap_or_default(),
                &format!("{registry}/wamn/{name}"),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn installation_requires_every_pinned_image_of_this_registry_and_chart_version() {
        let digest = "a".repeat(64);
        let mut values = json!({
            "runtime-operator": {"runtime": {"image": {"repository":"localhost:5001/wamn-host","tag":format!("1.2.3@sha256:{digest}")}}},
            "platform": {}
        });
        for name in ["runner", "gates", "identity"] {
            values["platform"][name] =
                json!({"image":format!("localhost:5001/wamn-{name}:1.2.3@sha256:{digest}")});
        }
        values["platform"]["edge"] =
            json!({"image": format!("localhost:5001/wamn-edge-web:1.2.3@sha256:{digest}")});
        values["platform"]["roles"] = json!({});
        for name in ["http", "materializer"] {
            values["platform"]["roles"][name] =
                json!({"image":format!("localhost:5001/wamn/{name}@sha256:{digest}")});
        }
        assert!(images_match(&values, "localhost:5001", "1.2.3"));
        assert!(!images_match(&values, "localhost:5002", "1.2.3"));
        assert!(!images_match(&values, "localhost:5001", "1.2.4"));
        let mut pointers = vec!["/runtime-operator/runtime/image/tag".to_owned()];
        for (name, image) in values["platform"].as_object().unwrap() {
            if image.get("image").is_some() {
                pointers.push(format!("/platform/{name}/image"));
            } else {
                pointers.extend(
                    image
                        .as_object()
                        .unwrap()
                        .keys()
                        .map(|role| format!("/platform/{name}/{role}/image")),
                );
            }
        }
        for pointer in pointers {
            let mut missing = values.clone();
            *missing.pointer_mut(&pointer).unwrap() = json!("");
            assert!(
                !images_match(&missing, "localhost:5001", "1.2.3"),
                "{pointer}"
            );
        }
    }
}
