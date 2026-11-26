use serde_json::Value;

pub fn assert_pod_resources(objects: &[Value]) {
    let mut pods = objects
        .iter()
        .filter(|object| object["spec"]["template"]["spec"].is_object())
        .peekable();
    assert!(pods.peek().is_some(), "the chart must render pod templates");
    for object in pods {
        let name = &object["metadata"]["name"];
        let pod = &object["spec"]["template"]["spec"];
        let containers = pod["containers"].as_array().expect("pod containers");
        assert!(!containers.is_empty(), "{name} has no containers");
        for container in containers
            .iter()
            .chain(pod["initContainers"].as_array().into_iter().flatten())
        {
            assert_compute_resources(
                &container["resources"],
                &format!("{name}/{}", container["name"]),
            );
        }
    }
}

pub fn assert_compute_resources(resources: &Value, workload: &str) {
    for (field, resource) in [
        ("requests", "cpu"),
        ("requests", "memory"),
        ("limits", "memory"),
    ] {
        assert!(
            resources[field][resource]
                .as_str()
                .is_some_and(|quantity| !quantity.is_empty()),
            "{workload} must set {field}.{resource}",
        );
    }
}
