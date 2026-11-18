wit_bindgen::generate!({
    world: "wamn:ingress-component/ingress@0.1.0",
    path: [
        "../../wit/wamn-ingress",
        "../../wit/wamn-postgres-0.4",
        "../../wit/wamn-node",
        "../../wit/wamn-operation-0.1",
        "../../wit/wasi-clocks",
        "../../wit/wasi-config",
        "wit",
    ],
    generate_all,
    async: [
        "export:wasi:http/handler@0.3.0#handle",
        "export:wasmcloud:nats/jetstream-handler@0.1.3#handle-message",
        "wamn:operation/operation@0.1.0#dispatch-data-change",
        "wamn:ingress/routing@0.1.0#authenticate",
        "wamn:operation/operation@0.1.0#invoke",
        "wamn:operation/operation@0.1.0#sign-result",
    ],
});
