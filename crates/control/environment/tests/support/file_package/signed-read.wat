;; Echo the JSON result through the existing native async node ABI.
(component
  (import "wamn:node/types@0.1.0" (instance $node
    (type $json' string)
    (export "json" (type $json (eq $json')))
    (type $context' (record
      (field "wiring-id" string) (field "wiring-version" u32)
      (field "node-id" string) (field "delivery-id" string)
      (field "input-port" (option string)) (field "occurrence" u32)
      (field "traceparent" (option string)) (field "tracestate" (option string))
      (field "deadline-ms" (option u64)) (field "config" $json)))
    (export "node-context" (type $context (eq $context')))
    (type $detail' (record (field "message" string) (field "code" (option string))))
    (export "error-detail" (type $detail (eq $detail')))
    (type $rate' (record (field "detail" $detail) (field "retry-after-ms" (option u64))))
    (export "rate-limit-detail" (type $rate (eq $rate')))
    (type $error' (variant (case "retryable" $detail) (case "rate-limited" $rate)
      (case "terminal" $detail) (case "invalid-input" $detail) (case "cancelled")))
    (export "node-error" (type $error (eq $error')))
    (type $emission' (record (field "payload" $json) (field "port" (option string))))
    (export "emission" (type $emission (eq $emission')))))
  (alias export $node "json" (type $json))
  (alias export $node "node-context" (type $context))
  (alias export $node "node-error" (type $error))
  (alias export $node "emission" (type $emission))
  (import "wasmcloud:blobstore/blobstore@0.1.0" (instance))
  (import "wamn:postgres/statements@0.4.0" (instance))
  (core module $memory
    (memory (export "memory") 16)
    (global $next (mut i32) (i32.const 1024))
    (func (export "realloc") (param $old i32) (param $old-size i32)
      (param $align i32) (param $size i32) (result i32) (local $new i32)
      global.get $next local.get $align i32.const 1 i32.sub i32.add
      i32.const 0 local.get $align i32.sub i32.and local.tee $new
      local.get $size i32.add global.set $next
      global.get $next i32.const 1048576 i32.gt_u if unreachable end
      local.get $old if
        local.get $new local.get $old local.get $old-size memory.copy
      end local.get $new))
  (core instance $memory (instantiate $memory))
  (core func $return (canon task.return (result (result $emission (error $error)))
    (memory $memory "memory")))
  (core module $main
    (import "memory" "memory" (memory 16))
    (import "host" "return" (func $return
      (param i32 i32 i32 i32 i32 i32 i32 i32 i64)))
    (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
    (func (export "run") (param $input i32) (result i32)
      i32.const 0 local.get $input i32.load offset=96 local.get $input i32.load offset=100
      i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i64.const 0
      call $return i32.const 0))
  (core instance $main (instantiate $main (with "memory" (instance $memory))
    (with "host" (instance (export "return" (func $return))))))
  (func $run async (param "ctx" $context) (param "input" $json)
    (result (result $emission (error $error)))
    (canon lift (core func $main "run") (memory $memory "memory")
      (realloc (func $memory "realloc")) async (callback (func $main "callback"))))
  (instance $handler
    (export "json" (type $json)) (export "node-context" (type $context))
    (export "node-error" (type $error)) (export "emission" (type $emission))
    (export "run" (func $run)))
  (export "file-fixture:document/signed-read@0.1.0" (instance $handler)))
