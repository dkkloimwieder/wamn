;; HTTP fixture. The host supplies only the private binding.
(module
  (import "wamn:connector/bound@0.1.0" "authority" (func $authority (param i32)))
  (import "wamn:connector/bound@0.1.0" "credential" (func $credential (param i32)))
  (import "wasi:sockets/instance-network@0.2.12" "instance-network" (func $network (result i32)))
  (import "wasi:sockets/tcp-create-socket@0.2.12" "create-tcp-socket" (func $create (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.12" "[method]tcp-socket.start-connect"
    (func $connect (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:sockets/tcp@0.2.12" "[method]tcp-socket.finish-connect" (func $finish (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.12" "[method]tcp-socket.subscribe" (func $subscribe (param i32) (result i32)))
  (import "wasi:io/poll@0.2.12" "[method]pollable.block" (func $block (param i32)))
  (import "wasi:io/poll@0.2.12" "[resource-drop]pollable" (func $drop-poll (param i32)))
  (import "wasi:io/streams@0.2.12" "[method]output-stream.blocking-write-and-flush" (func $write (param i32 i32 i32 i32)))
  (import "wasi:io/streams@0.2.12" "[method]input-stream.blocking-read" (func $read (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.12" "[resource-drop]input-stream" (func $drop-input (param i32)))
  (import "wasi:io/streams@0.2.12" "[resource-drop]output-stream" (func $drop-output (param i32)))
  (import "wasi:sockets/tcp@0.2.12" "[resource-drop]tcp-socket" (func $drop-socket (param i32)))
  (import "wasi:sockets/network@0.2.12" "[resource-drop]network" (func $drop-network (param i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "\47\45\54\20\2f\73\61\6d\70\6c\65\20\48\54\54\50\2f\31\2e\31\0d\0a\48\6f\73\74\3a\20\66\69\78\74\75\72\65\0d\0a\41\75\74\68\6f\72\69\7a\61\74\69\6f\6e\3a\20\42\65\61\72\65\72\20")
  (data (i32.const 1200) "\0d\0a\43\6f\6e\6e\65\63\74\69\6f\6e\3a\20\63\6c\6f\73\65\0d\0a\0d\0a")
  (data (i32.const 1400) "socket failure")
  (global $heap (mut i32) (i32.const 4096))
  (func (export "cabi_realloc") (param i32 i32 i32) (param $len i32) (result i32)
    (local $ptr i32)
    global.get $heap local.tee $ptr local.get $len i32.add i32.const 7 i32.add i32.const -8 i32.and global.set $heap
    local.get $ptr)
  (func $fail (param $code i32) (result i32)
    i32.const 128 i32.const 1 i32.store8
    (if (i32.eq (local.get $code) (i32.const 1))
      (then i32.const 132 i32.const 2 i32.store8)
      (else
        i32.const 132 i32.const 5 i32.store8
        i32.const 136 i32.const 1400 i32.store
        i32.const 140 i32.const 14 i32.store))
    i32.const 128)
  (func (export "wamn:connector/reader@0.1.0#get") (param i32 i32) (param $request i32) (param $request-len i32) (result i32)
    (local $port i32) (local $cursor i32) (local $end i32) (local $net i32) (local $socket i32) (local $poll i32)
    (local $input i32) (local $output i32) (local $code i32) (local $status i32) (local $length i32)
    ;; The request contains a NUL and bytes that are not valid UTF-8.
    (if (i32.or (i32.ne (local.get $request-len) (i32.const 4))
                 (i32.ne (i32.load (local.get $request)) (i32.const 0x0180ff00)))
      (then i32.const 1000 call $fail return))
    i32.const 4096 global.set $heap
    i32.const 160 call $authority
    i32.const 160 i32.load local.set $cursor
    local.get $cursor i32.const 164 i32.load i32.add local.set $end
    (block $parsed (loop $digit
      local.get $cursor local.get $end i32.ge_u br_if $parsed
      (if (i32.eq (i32.load8_u (local.get $cursor)) (i32.const 58))
        (then i32.const 0 local.set $port)
        (else local.get $port i32.const 10 i32.mul local.get $cursor i32.load8_u i32.const 48 i32.sub i32.add local.set $port))
      local.get $cursor i32.const 1 i32.add local.set $cursor
      br $digit))
    call $network local.set $net
    i32.const 0 i32.const 16 call $create
    (if (i32.load8_u (i32.const 16)) (then
      local.get $net call $drop-network
      (i32.load8_u (i32.const 20)) call $fail return))
    (i32.load (i32.const 20)) local.set $socket
    local.get $socket local.get $net i32.const 0
    local.get $port i32.const PORT_DELTA i32.add
    i32.const HOST_A i32.const HOST_B i32.const HOST_C i32.const HOST_D
    i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
    i32.const 32 call $connect
    (if (i32.load8_u (i32.const 32)) (then
      (i32.load8_u (i32.const 33)) local.set $code
      local.get $socket call $drop-socket local.get $net call $drop-network
      local.get $code call $fail return))
    local.get $socket call $subscribe local.set $poll
    local.get $poll call $block
    local.get $poll call $drop-poll
    local.get $socket i32.const 48 call $finish
    (if (i32.load8_u (i32.const 48)) (then
      (i32.load8_u (i32.const 52)) local.set $code
      local.get $socket call $drop-socket local.get $net call $drop-network
      local.get $code call $fail return))
    (i32.load (i32.const 52)) local.set $input
    (i32.load (i32.const 56)) local.set $output
    i32.const 64 call $credential
    local.get $output i32.const 1024 i32.const 59 i32.const 80 call $write
    local.get $output (i32.load (i32.const 68)) (i32.load (i32.const 72)) i32.const 80 call $write
    local.get $output i32.const 1200 i32.const 23 i32.const 80 call $write
    i32.const 1000 local.set $status
    (if (i32.eqz (i32.load8_u (i32.const 80))) (then
      ;; Read until the complete HTTP status line is present, even if TCP fragments it.
      (block $done (loop $more
        local.get $input i64.const 4096 i32.const 96 call $read
        (br_if $done (i32.load8_u (i32.const 96)))
        ;; Only the first twelve bytes are needed for HTTP/1.1 NNN.
        i32.const 512 local.get $length i32.add
        (i32.load (i32.const 100))
        (if (result i32) (i32.lt_u (i32.load (i32.const 104)) (i32.sub (i32.const 12) (local.get $length)))
          (then (i32.load (i32.const 104))) (else i32.const 12 local.get $length i32.sub))
        local.tee $code memory.copy
        local.get $length local.get $code i32.add local.set $length
        (if (i32.ge_u (local.get $length) (i32.const 12)) (then
          (i32.load8_u (i32.const 521)) i32.const 48 i32.sub i32.const 100 i32.mul
          (i32.load8_u (i32.const 522)) i32.const 48 i32.sub i32.const 10 i32.mul i32.add
          (i32.load8_u (i32.const 523)) i32.const 48 i32.sub i32.add local.set $status
          br $done))
        br $more))))
    local.get $input call $drop-input local.get $output call $drop-output
    local.get $socket call $drop-socket local.get $net call $drop-network
    (if (i32.eq (local.get $status) (i32.const 1000)) (then i32.const 1000 call $fail return))
    i32.const 128 i32.const 0 i32.store8
    i32.const 132 i32.const 521 i32.store
    ;; Return the status and two binary bytes.
    i32.const 524 i32.const 0 i32.store8
    i32.const 525 i32.const 255 i32.store8
    i32.const 136 i32.const 5 i32.store
    i32.const 128))
