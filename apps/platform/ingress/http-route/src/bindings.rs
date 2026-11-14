wit_bindgen::generate!({
    world: "wamn:flow-http/flow-http@0.1.0",
    path: [
        "../../../../wit/wamn-flow-http-routing",
        "../../../../wit/wamn-postgres-0.4",
        "../../../../wit/wamn-node",
        "../../../../wit/wamn-operation-0.1",
        "../../execution/materializer/wit/deps/wasi-clocks",
        "../../../../wit/wasi-config",
        "wit",
    ],
    generate_all,
    async: [
        "export:wasi:http/handler@0.3.0#handle",
        "wamn:flow-http-routing/routing@0.1.0#authenticate",
        "wamn:operation/operation@0.1.0#invoke",
        "wamn:operation/operation@0.1.0#sign-result",
    ],
});
