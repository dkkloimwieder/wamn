use serde_json::Value;

pub fn assert_restricted_pods<'a>(objects: impl Iterator<Item = &'a Value>) {
    let mut pods = objects.peekable();
    assert!(pods.peek().is_some(), "the chart must render pod templates");
    for object in pods {
        let name = &object["metadata"]["name"];
        let pod = &object["spec"]["template"]["spec"];
        let context = &pod["securityContext"];
        assert_eq!(context["runAsNonRoot"], true, "{name}");
        assert!(
            context["runAsUser"].as_u64().is_some_and(|uid| uid > 0),
            "{name} needs a nonzero numeric UID"
        );
        assert!(
            context["runAsGroup"].as_u64().is_some_and(|gid| gid > 0),
            "{name} needs a nonzero numeric GID"
        );
        assert_eq!(context["fsGroup"], context["runAsGroup"], "{name}");
        assert_eq!(
            context["seccompProfile"]["type"], "RuntimeDefault",
            "{name}"
        );

        let containers = pod["containers"].as_array().expect("pod containers");
        assert!(!containers.is_empty(), "{name} has no containers");
        for container in containers
            .iter()
            .chain(pod["initContainers"].as_array().into_iter().flatten())
        {
            let container_name = &container["name"];
            let context = &container["securityContext"];
            assert_eq!(
                context["allowPrivilegeEscalation"], false,
                "{name}/{container_name}"
            );
            assert_eq!(
                context["capabilities"]["drop"],
                serde_json::json!(["ALL"]),
                "{name}/{container_name}"
            );
        }
    }
}
