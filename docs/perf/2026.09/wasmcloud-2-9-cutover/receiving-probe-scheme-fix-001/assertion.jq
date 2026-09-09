    .spec.replicas == 1 and .status.availableReplicas == 1 and
    (.spec.template.spec.containers | length == 1) and
    .spec.template.spec.containers[0].name == "host" and
    .spec.template.spec.containers[0].livenessProbe.httpGet == {path:"/livez",port:"probes",scheme:"HTTP"} and
    .spec.template.spec.containers[0].readinessProbe.httpGet == {path:"/readyz",port:"probes",scheme:"HTTP"} and
    .spec.template.spec.containers[0].startupProbe.httpGet == {path:"/livez",port:"probes",scheme:"HTTP"} and
    any(.spec.template.spec.containers[0].ports[]; .name == "probes" and .containerPort == 8081)
