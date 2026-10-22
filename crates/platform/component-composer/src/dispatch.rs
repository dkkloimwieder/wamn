//! Generate the private selector from the application's operation exports.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use wac_graph::types::{DefinedType, ItemKind, Package, Types, ValueType};

use anyhow::{Context as _, ensure};
use wit_parser::{
    LiftLowerAbi, ManglingAndAbi, Resolve, SizeAlign, TypeDefKind, WasmExport, WasmExportKind,
    WasmImport, WorldItem, WorldKey,
};

const CONTRACT: &str = include_str!("../wit/package.wit");

pub(super) fn build(application: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut types = Types::default();
    let application = Package::from_bytes("application", None, application, &mut types)
        .context("read application operation exports")?;
    let mut resolve = Resolve::default();
    resolve.push_str(
        "node.wit",
        include_str!("../../../execution/workflow/router/wit/package.wit"),
    )?;
    let mut packages = BTreeMap::<String, String>::new();
    let mut names = Vec::new();
    for (name, item) in &types[application.ty()].exports {
        let ItemKind::Instance(id) = item else {
            continue;
        };
        let interface = &types[*id];
        let Some((function_name, ItemKind::Func(function))) = interface
            .exports
            .get_key_value("run-json")
            .or_else(|| interface.exports.get_key_value("run"))
        else {
            continue;
        };
        let projection = interface.exports.contains_key("read-json");
        let function = &types[*function];
        let result = function.result.context("operation has no result")?;
        let query = is_query(&types, result);
        let result = if query {
            "tuple<future<query-metadata>, stream<string>, future<string>>"
        } else {
            "emission"
        };
        let (package, interface) = name.split_once('/').context("operation has no interface")?;
        let (interface, version) = interface
            .split_once('@')
            .context("operation has no version")?;
        let source = packages.entry(format!("{package}@{version}")).or_default();
        let asynchronous = if function.is_async { "async " } else { "" };
        let parameters = function.params.keys().collect::<Vec<_>>();
        ensure!(
            parameters.len() == if query { 3 } else { 2 },
            "operation {name} does not use the node JSON call ABI"
        );
        let metadata = if query {
            "record query-metadata { wal-position: option<string>, }"
        } else {
            ""
        };
        let wal = if query {
            ", want-wal-position: bool"
        } else {
            ""
        };
        let read_json = if projection {
            "read-json: async func(ctx: node-context, input: string, want-wal-position: bool) -> result<tuple<emission, option<string>>, node-error>;"
        } else {
            ""
        };
        writeln!(source,
            "interface %{interface} {{
                use wamn:node/types@0.1.0.{{node-context, emission, node-error}};
                {metadata}
                {function_name}: {asynchronous}func(%{}: node-context, %{}: string{wal}) -> result<{result}, node-error>;
                {read_json}
            }}", parameters[0], parameters[1])?;
        names.push(name.clone());
    }
    // Only reconstruct the admitted JSON seam. Composition checks its types
    // against the application's exports; named SQL imports never enter Resolve.
    for (package, source) in packages {
        resolve.push_str("operations.wit", &format!("package {package}; {source}"))?;
    }
    names.sort();
    let mut operations = Vec::new();
    for name in names {
        let id = resolve
            .interfaces
            .iter()
            .find_map(|(id, _)| {
                (resolve.name_world_key(&WorldKey::Interface(id)) == name).then_some(id)
            })
            .context("operation interface missing")?;
        operations.push((name, id));
    }
    let mut imports = String::new();
    for (name, _) in &operations {
        let (package, interface) = name.split_once('/').context("operation has no interface")?;
        write!(imports, "import {package}/%{interface};")?;
    }
    // Parsing the world elaborates the shared node types before their users.
    let package = resolve.push_str(
        "dispatch.wit",
        &CONTRACT.replace("export dispatch;", &format!("{imports} export dispatch;")),
    )?;
    let world = resolve.select_world(&[package], Some("adapter"))?;
    let sync = ManglingAndAbi::Legacy(LiftLowerAbi::Sync);
    let callback_abi = ManglingAndAbi::Legacy(LiftLowerAbi::AsyncCallback);
    let mut sizes = SizeAlign::default();
    sizes.fill(&resolve);
    let mut module = String::from("(module\n");
    let mut calls = [String::new(), String::new(), String::new()];
    let mut data = String::new();
    let mut queries = String::new();
    let mut projections = String::new();
    let mut offset = 16_usize;
    for (index, (name, id)) in operations.iter().enumerate() {
        for (function_index, function) in resolve.interfaces[*id].functions.values().enumerate() {
            let projection = function.name == "read-json";
            let signature =
                resolve.wasm_signature(wit_parser::abi::AbiVariant::GuestImport, function);
            ensure!(
                signature.indirect_params && signature.retptr && signature.params.len() == 2,
                "operation {name} does not use the node JSON call ABI"
            );
            let key = WorldKey::Interface(*id);
            let (namespace, import) = resolve.wasm_import_name(
                sync,
                WasmImport::Func {
                    interface: Some(&key),
                    func: function,
                },
            );
            writeln!(
                module,
                "(import {namespace:?} {import:?} (func $op{index}_{function_index} (param i32 i32)))"
            )?;
            let result = function.result.context("operation has no result")?;
            let query = is_wit_query(&resolve, result);
            let call_index = if projection { 2 } else { usize::from(query) };
            writeln!(calls[call_index],
            "local.get $args i32.load local.get $args i32.load offset=4 i32.const {offset} i32.const {} call $equal
             if local.get $args i32.const 8 i32.add local.get $result call $op{index}_{function_index}
                call $finish{} i32.const 0 return end",
            name.len(), call_index)?;
            if query {
                writeln!(
                    queries,
                    "local.get 0 local.get 1 i32.const {offset} i32.const {} call $equal if i32.const 1 return end",
                    name.len()
                )?;
            }
            if projection {
                writeln!(
                    projections,
                    "local.get 0 local.get 1 i32.const {offset} i32.const {} call $equal if i32.const 1 return end",
                    name.len()
                )?;
            }
        }
        writeln!(data, "(data (i32.const {offset}) {name:?})")?;
        offset += name.len();
    }
    let (key, id) = resolve.worlds[world]
        .exports
        .iter()
        .find_map(|(key, item)| match item {
            WorldItem::Interface { id, .. } => Some((key, *id)),
            _ => None,
        })
        .context("dispatch interface missing")?;
    let mut exports = String::new();
    for (name, selectors) in [("is-query", queries), ("is-projection", projections)] {
        let function = &resolve.interfaces[id].functions[name];
        let export = resolve.wasm_export_name(
            sync,
            WasmExport::Func {
                interface: Some(key),
                func: function,
                kind: WasmExportKind::Normal,
            },
        );
        writeln!(
            exports,
            "(func (export {export:?}) (param i32 i32) (result i32) {selectors} i32.const 0)"
        )?;
    }
    for (index, name) in ["run", "query", "read"].iter().enumerate() {
        let function = &resolve.interfaces[id].functions[*name];
        let (namespace, import, signature) =
            function.task_return_import(&resolve, Some(key), callback_abi.mangling());
        let parameters = signature
            .params
            .iter()
            .map(|ty| core_type(*ty))
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(
            module,
            "(import {namespace:?} {import:?} (func $return{index} (param {parameters})))"
        )?;
        let export = resolve.wasm_export_name(
            callback_abi,
            WasmExport::Func {
                interface: Some(key),
                func: function,
                kind: WasmExportKind::Normal,
            },
        );
        let result_size = sizes.size(&function.result.unwrap()).size_wasm32();
        let lifted = lift(&resolve, &sizes, function.result.unwrap(), 0)?;
        writeln!(
            exports,
            "(func $finish{index} (param $result i32) {} call $return{index})",
            lifted
                .into_iter()
                .map(|(_, code)| code)
                .collect::<Vec<_>>()
                .join(" ")
        )?;
        let callback = resolve.wasm_export_name(
            callback_abi,
            WasmExport::Func {
                interface: Some(key),
                func: function,
                kind: WasmExportKind::Callback,
            },
        );
        writeln!(
            exports,
            "(func (export {callback:?}) (param i32 i32 i32) (result i32) unreachable)"
        )?;
        // Calls supply their result pointer to the task-return adapter.
        calls[index] = calls[index].replace(
            &format!("call $finish{index}"),
            &format!("local.get $result call $finish{index}"),
        );
        writeln!(exports,
            "(func (export {export:?}) (param $args i32) (result i32) (local $result i32)
                i32.const 0 i32.const 0 i32.const 8 i32.const {result_size} call $realloc local.set $result
                {} unreachable)", calls[index])?;
    }
    // The dispatcher lives for one HTTP Store. Its arena holds arguments and
    // results until that Store drops, including live stream and future handles.
    let heap = (offset + 7) & !7;
    writeln!(
        module,
        "(memory (export \"memory\") {}) (global $heap (mut i32) (i32.const {heap}))",
        heap.div_ceil(65536)
    )?;
    module.push_str(include_str!("dispatch.wat"));
    module.push_str(&data);
    module.push_str(&exports);
    module.push(')');
    let mut bytes = wat::parse_str(&module).context("encode dispatch module")?;
    wit_component::embed_component_metadata(
        &mut bytes,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
    )?;
    wit_component::ComponentEncoder::default()
        .module(&bytes)?
        .validate(true)
        .encode()
        .context("encode private application dispatcher")
}

fn core_type(ty: wit_parser::abi::WasmType) -> &'static str {
    use wit_parser::abi::WasmType;
    match ty {
        WasmType::I64 | WasmType::PointerOrI64 => "i64",
        WasmType::F32 => "f32",
        WasmType::F64 => "f64",
        _ => "i32",
    }
}

/// Flatten the dispatch result types using their canonical memory layout.
fn lift(
    resolve: &Resolve,
    sizes: &SizeAlign,
    ty: wit_parser::Type,
    offset: usize,
) -> anyhow::Result<Vec<(&'static str, String)>> {
    use wit_parser::{Int, Type};
    let load = |kind, instruction| {
        (
            kind,
            format!("local.get $result {instruction} offset={offset}"),
        )
    };
    let fields = |types: Vec<Type>| -> anyhow::Result<Vec<_>> {
        sizes
            .field_offsets(&types)
            .into_iter()
            .try_fold(Vec::new(), |mut flat, (position, ty)| {
                flat.extend(lift(resolve, sizes, *ty, offset + position.size_wasm32())?);
                Ok(flat)
            })
    };
    let cases = match ty {
        Type::String => {
            return Ok(vec![
                load("i32", "i32.load"),
                (
                    "i32",
                    format!("local.get $result i32.load offset={}", offset + 4),
                ),
            ]);
        }
        Type::U64 => return Ok(vec![load("i64", "i64.load")]),
        Type::Id(id) => match &resolve.types[id].kind {
            TypeDefKind::Type(ty) => return lift(resolve, sizes, *ty, offset),
            TypeDefKind::Record(record) => {
                return fields(record.fields.iter().map(|field| field.ty).collect());
            }
            TypeDefKind::Tuple(tuple) => return fields(tuple.types.clone()),
            TypeDefKind::Stream(_) | TypeDefKind::Future(_) => {
                return Ok(vec![load("i32", "i32.load")]);
            }
            TypeDefKind::Variant(variant) => {
                variant.cases.iter().map(|case| case.ty).collect::<Vec<_>>()
            }
            TypeDefKind::Option(ty) => vec![None, Some(*ty)],
            TypeDefKind::Result(result) => vec![result.ok, result.err],
            _ => anyhow::bail!("unsupported dispatch result type: {ty:?}"),
        },
        _ => anyhow::bail!("unsupported dispatch result primitive: {ty:?}"),
    };
    ensure!(cases.len() <= 256, "dispatch result has too many cases");
    let payload = offset
        + sizes
            .payload_offset(Int::U8, cases.iter().map(Option::as_ref))
            .size_wasm32();
    let values = cases
        .iter()
        .map(|case| {
            case.map(|ty| lift(resolve, sizes, ty, payload))
                .transpose()
                .map(Option::unwrap_or_default)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut storage = [wit_parser::abi::WasmType::I32; 16];
    let mut flat = wit_parser::abi::FlatTypes::new(&mut storage);
    ensure!(
        resolve.push_flat(&ty, &mut flat),
        "dispatch result exceeds flat task return"
    );
    let flat = flat.to_vec();
    let mut output = vec![load("i32", "i32.load8_u")];
    for (slot, kind) in flat.iter().skip(1).map(|ty| core_type(*ty)).enumerate() {
        let mut code = format!("{kind}.const 0");
        for (tag, value) in values.iter().enumerate().rev() {
            let arm = match value.get(slot) {
                Some((actual, value)) if *actual == kind => value.clone(),
                Some(("i32", value)) if kind == "i64" => format!("{value} i64.extend_i32_u"),
                Some(_) => anyhow::bail!("unsupported dispatch result join"),
                None => format!("{kind}.const 0"),
            };
            code = format!(
                "local.get $result i32.load8_u offset={offset} i32.const {tag} i32.eq if (result {kind}) {arm} else {code} end"
            );
        }
        output.push((kind, code));
    }
    Ok(output)
}

fn is_wit_query(resolve: &Resolve, result: wit_parser::Type) -> bool {
    let wit_parser::Type::Id(id) = result else {
        return false;
    };
    match &resolve.types[id].kind {
        TypeDefKind::Result(result) => result.ok.is_some_and(|ty| is_wit_query(resolve, ty)),
        TypeDefKind::Tuple(tuple) => tuple.types.iter().any(|ty| is_wit_query(resolve, *ty)),
        TypeDefKind::Stream(_) => true,
        TypeDefKind::Type(ty) => is_wit_query(resolve, *ty),
        _ => false,
    }
}

fn is_query(types: &Types, result: ValueType) -> bool {
    let ValueType::Defined(id) = result else {
        return false;
    };
    match &types[id] {
        DefinedType::Result { ok, .. } => ok.is_some_and(|ty| is_query(types, ty)),
        DefinedType::Tuple(tuple) => tuple.iter().any(|ty| is_query(types, *ty)),
        DefinedType::Stream(_) => true,
        DefinedType::Alias(ty) => is_query(types, *ty),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_named_imports_without_decoding_them_as_wit() {
        let application = wat::parse_str(
            r#"(component
            (type $sql (instance
                (type $row (record (field "value" string)))
                (export "row" (type (eq $row)))
            ))
            (import "sql-0" (implements "wamn:postgres/statements@0.4.0") (instance (type $sql)))
            (import "sql-1" (implements "wamn:postgres/statements@0.4.0") (instance (type $sql)))
        )"#,
        )
        .unwrap();
        super::build(&application).expect("named imports do not enter the WIT decoder");
    }

    #[test]
    fn generates_dispatch_for_async_json_and_query_exports() {
        let mut resolve = wit_parser::Resolve::default();
        resolve
            .push_str(
                "node.wit",
                include_str!("../../../execution/workflow/router/wit/package.wit"),
            )
            .unwrap();
        let package = resolve.push_str("app.wit", r"
            package test:app@1.0.0;
            interface get {
                use wamn:node/types@0.1.0.{node-context, emission, node-error};
                run-json: async func(ctx: node-context, input: string) -> result<emission, node-error>;
            }
            interface query {
                use wamn:node/types@0.1.0.{node-context, node-error};
                record query-metadata { wal-position: option<string>, }
                run-json: async func(ctx: node-context, input: string, want-wal-position: bool) -> result<tuple<future<query-metadata>, stream<string>, future<string>>, node-error>;
            }
            interface projection {
                use wamn:node/types@0.1.0.{node-context, emission, node-error};
                run-json: async func(ctx: node-context, input: string) -> result<emission, node-error>;
                read-json: async func(ctx: node-context, input: string, want-wal-position: bool) -> result<tuple<emission, option<string>>, node-error>;
            }
            world app { export get; export query; export projection; }
        ").unwrap();
        let world = resolve.select_world(&[package], None).unwrap();
        let mut module =
            wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
        wit_component::embed_component_metadata(
            &mut module,
            &resolve,
            world,
            wit_component::StringEncoding::UTF8,
        )
        .unwrap();
        let app = wit_component::ComponentEncoder::default()
            .module(&module)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap();
        let dispatcher = super::build(&app).expect("generate a valid dispatcher");
        assert!(
            super::super::names(&dispatcher, "dispatch", true)
                .unwrap()
                .contains(super::super::APPLICATION_DISPATCH)
        );
    }
}
