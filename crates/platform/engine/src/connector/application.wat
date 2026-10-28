;; The application forwards a logical request. It imports no binding values.
(module
  (import "wamn:connector/reader@0.1.0" "get" (func $get (param i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 2048) "\00\ff\80\01")
  (data (i32.const 2080) "\22unbound\22")
  (global $heap (mut i32) (i32.const 4096))
  (func (export "cabi_realloc") (param i32 i32 i32) (param $len i32) (result i32)
    (local $ptr i32)
    global.get $heap local.tee $ptr local.get $len i32.add i32.const 7 i32.add i32.const -8 i32.and global.set $heap
    local.get $ptr)
  ;; The canonical ABI passes the large node context and input indirectly.
  (func (export "wamn:node/handler@0.1.0#run") (param $args i32) (result i32)
    local.get $args i32.load offset=96
    local.get $args i32.load offset=100
    i32.const 2048 i32.const 4
    i32.const 16 call $get
    (if (i32.load8_u (i32.const 16)) (then
      ;; Only the unbound case can produce this emission.
      (if (i32.load8_u (i32.const 20)) (then unreachable))
      i32.const 128 i32.const 0 i32.store8
      i32.const 136 i32.const 2080 i32.store
      i32.const 140 i32.const 9 i32.store
      i32.const 144 i32.const 0 i32.store8
      i32.const 128 return))
    ;; The reply contains the status followed by NUL and 0xff.
    (if (i32.or (i32.ne (i32.load (i32.const 24)) (i32.const 5))
        (i32.or (i32.load8_u offset=3 (i32.load (i32.const 20)))
          (i32.ne (i32.load8_u offset=4 (i32.load (i32.const 20))) (i32.const 255))))
      (then unreachable))
    ;; The node result has eight-byte alignment because its error holds a u64.
    i32.const 128 i32.const 0 i32.store8
    i32.const 136 (i32.load (i32.const 20)) i32.store
    i32.const 140 i32.const 3 i32.store
    i32.const 144 i32.const 0 i32.store8
    i32.const 128))
