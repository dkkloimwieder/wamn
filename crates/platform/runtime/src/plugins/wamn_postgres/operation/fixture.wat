(component
  (import "wamn:postgres/types@0.4.0" (instance $types
    (type $sql-value' (variant (case "null") (case "boolean" bool) (case "int32" s32)
      (case "int64" s64) (case "float64" f64) (case "text" string) (case "bytes" (list u8))
      (case "numeric" string) (case "timestamptz" string) (case "json" string) (case "uuid" string)))
    (export "sql-value" (type $sql-value (eq $sql-value')))
    (type $column' (record (field "name" string) (field "type-name" string)))
    (export "column" (type $column (eq $column')))
    (type $row-set' (record (field "columns" (list $column)) (field "rows" (list (list $sql-value)))))
    (export "row-set" (type $row-set (eq $row-set')))
    (type $pg-error' (variant (case "serialization-failure") (case "connection-unavailable")
      (case "statement-timeout") (case "row-limit-exceeded" u64) (case "unique-violation" string)
      (case "foreign-key-violation" string) (case "check-violation" string)
      (case "exclusion-violation" string) (case "permission-denied")
      (case "query-error" (tuple string string))))
    (export "pg-error" (type $pg-error (eq $pg-error')))))
  (alias export $types "sql-value" (type $sql-value))
  (alias export $types "column" (type $column))
  (alias export $types "row-set" (type $row-set))
  (alias export $types "pg-error" (type $pg-error))
  (import "wamn:postgres/statements@0.4.0" (instance $statements
    (type $contract-part' (enum "binds" "columns"))
    (export "contract-part" (type $contract-part (eq $contract-part')))
    (type $value-shape' (record (field "count" u32) (field "types" (list string))))
    (export "value-shape" (type $value-shape (eq $value-shape')))
    (type $contract-mismatch' (record (field "statement" string) (field "part" $contract-part)
      (field "expected" $value-shape) (field "observed" $value-shape)))
    (export "contract-mismatch" (type $contract-mismatch (eq $contract-mismatch')))
    (type $statement-error' (variant (case "unknown-statement" string)
      (case "statement-contract-mismatch" $contract-mismatch) (case "postgres" $pg-error)))
    (export "statement-error" (type $statement-error (eq $statement-error')))
    (export "transaction" (type $transaction (sub resource)))
    (export "operation-transaction" (func async
      (result (result (own $transaction) (error $statement-error)))))
    (export "begin" (func async
      (result (result (own $transaction) (error $statement-error)))))
    (export "run" (func async (param "statement" string) (param "binds" (list $sql-value))
      (result (result $row-set (error $statement-error)))))
    (export "run-stream" (func async (param "statement" string) (param "binds" (list $sql-value))
      (result (result (tuple (list $column) (stream (list $sql-value))
        (future (result (error $statement-error)))) (error $statement-error)))))
    (export "[method]transaction.run" (func async (param "self" (borrow $transaction))
      (param "statement" string) (param "binds" (list $sql-value))
      (result (result $row-set (error $statement-error)))))
    (export "[method]transaction.commit" (func async
      (param "self" (borrow $transaction)) (result (result (error $statement-error)))))))
  (alias export $statements "transaction" (type $transaction))
  (import "wamn:flow-http-routing/routing@0.1.0" (instance $routing
    (export "authenticated-caller" (type $caller (sub resource)))))
  (alias export $routing "authenticated-caller" (type $caller))
  (import "wamn:postgres/operation@0.4.0" (instance $operation
    (type $id' string)
    (export "sealed-operation-id" (type $id (eq $id')))
    (type $refusal' (variant (case "uncertain") (case "resolved") (case "conflict")
      (case "unknown-operation") (case "grant-denied") (case "invalid-caller")
      (case "not-begun") (case "already-begun") (case "postgres" $pg-error)))
    (export "refusal" (type $refusal (eq $refusal')))
    (type $begun' (variant (case "new") (case "finished" string)))
    (export "begun" (type $begun (eq $begun')))
    (export "begin-operation" (func async (param "caller" (borrow $caller))
      (param "operation" $id) (param "idempotency-key" string) (param "request-hash" (list u8))
      (result (result $begun (error $refusal)))))
    (export "commit" (func async (param "result" string) (result (result (error $refusal)))))))
  (core module $memory
    (memory (export "memory") 1)
    (data (i32.const 512) "base:widget/update@1.0.0")
    (data (i32.const 544) "key")
    (data (i32.const 560) "\ff\00\80")
    (data (i32.const 576) "update.sql")
    (data (i32.const 608) "{ \22value\22 : 7 }")
    (global $next (mut i32) (i32.const 1024))
    (func (export "realloc") (param i32 i32) (param $align i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $next local.get $align i32.const 1 i32.sub i32.add
      i32.const 0 local.get $align i32.sub i32.and
      local.tee $ptr local.get $size i32.add global.set $next local.get $ptr))
  (core instance $memory (instantiate $memory))
  (core func $begin (canon lower (func $operation "begin-operation")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $commit (canon lower (func $operation "commit")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $transaction (canon lower (func $statements "operation-transaction")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $run (canon lower (func $statements "[method]transaction.run")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $return (canon task.return (result bool)))
  (core module $main
    (import "memory" "memory" (memory 1))
    (import "host" "begin" (func $begin (param i32 i32 i32 i32 i32 i32 i32 i32)))
    (import "host" "commit" (func $commit (param i32 i32 i32)))
    (import "host" "transaction" (func $transaction (param i32)))
    (import "host" "run" (func $run (param i32 i32 i32 i32 i32 i32)))
    (import "host" "return" (func $return (param i32)))
    (func $begin-call (param $caller i32)
      local.get $caller i32.const 512 i32.const 24 i32.const 544 i32.const 3
      i32.const 560 i32.const 3 i32.const 64 call $begin)
    (func $commit-call
      i32.const 608 i32.const 15 i32.const 64 call $commit)
    (func $run-statement
      i32.const 128 call $transaction
      i32.const 128 i32.load8_u if unreachable end
      i32.const 136 i32.load i32.const 576 i32.const 10
      i32.const 0 i32.const 0 i32.const 192 call $run
      i32.const 192 i32.load8_u if unreachable end)
    (func $refused (param $tag i32)
      i32.const 64 i32.load8_u i32.const 1 i32.ne if unreachable end
      i32.const 72 i32.load8_u local.get $tag i32.ne if unreachable end)
    (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
    (func (export "run") (param $caller i32) (param $mode i32) (result i32)
      ;; Continue the same Store after the host observes the first delivery id.
      local.get $mode i32.const 9 i32.eq if
        call $commit-call
        i32.const 64 i32.load8_u if unreachable end
        i32.const 544 i32.const 110 i32.store8
      end
      local.get $mode i32.const 10 i32.eq if
        call $commit-call
        i32.const 64 i32.load8_u if unreachable end
        i32.const 1 call $return i32.const 0 return
      end
      local.get $mode i32.const 11 i32.eq if
        ;; An unknown operation refuses before opening a transaction.
        i32.const 512 i32.const 120 i32.store8
        local.get $caller call $begin-call i32.const 3 call $refused
        i32.const 512 i32.const 98 i32.store8
      end
      local.get $mode i32.eqz if
        call $commit-call i32.const 6 call $refused
      else
        local.get $caller call $begin-call
        local.get $mode i32.const 3 i32.eq if
          i32.const 4 call $refused
        else
          local.get $mode i32.const 6 i32.eq if
            i32.const 2 call $refused
          else
            local.get $mode i32.const 7 i32.eq if
              i32.const 0 call $refused
            else
              i32.const 64 i32.load8_u if unreachable end
              local.get $mode i32.const 5 i32.eq if
                i32.const 72 i32.load8_u i32.const 1 i32.ne if unreachable end
                ;; The stored text keeps the application's whitespace.
                i32.const 80 i32.load i32.const 15 i32.ne if unreachable end
                i32.const 76 i32.load i32.load i32.const 0x7622207b i32.ne if unreachable end
                ;; Finished leaves no open operation: another replay is allowed.
                local.get $caller call $begin-call
                i32.const 64 i32.load8_u if unreachable end
                i32.const 72 i32.load8_u i32.const 1 i32.ne if unreachable end
                ;; A replay cannot commit or run a statement.
                call $commit-call i32.const 6 call $refused
              else
                i32.const 72 i32.load8_u if unreachable end
                local.get $mode i32.const 1 i32.eq if
                  call $run-statement
                  local.get $caller call $begin-call i32.const 7 call $refused
                  ;; The refused begin left the open operation untouched.
                  call $run-statement
                  call $commit-call
                  i32.const 64 i32.load8_u if unreachable end
                  i32.const 544 i32.const 110 i32.store8
                  local.get $caller call $begin-call
                  i32.const 64 i32.load8_u if unreachable end
                  i32.const 72 i32.load8_u if unreachable end
                end
                call $run-statement
                local.get $mode i32.const 2 i32.ne
                local.get $mode i32.const 8 i32.ne i32.and
                local.get $mode i32.const 9 i32.ne i32.and if
                  call $commit-call
                  local.get $mode i32.const 12 i32.eq if
                    i32.const 8 call $refused
                    i32.const 544 i32.const 110 i32.store8
                    local.get $caller call $begin-call
                    i32.const 64 i32.load8_u if unreachable end
                    i32.const 72 i32.load8_u if unreachable end
                    call $run-statement
                    call $commit-call
                    i32.const 64 i32.load8_u if unreachable end
                  else
                    i32.const 64 i32.load8_u if unreachable end
                  end
                end
              end
            end
          end
        end
      end
      i32.const 1 call $return i32.const 0))
  (core instance $main (instantiate $main
    (with "memory" (instance $memory))
    (with "host" (instance (export "begin" (func $begin)) (export "commit" (func $commit))
      (export "transaction" (func $transaction)) (export "run" (func $run))
      (export "return" (func $return))))))
  (func (export "run") async (param "caller" (own $caller)) (param "mode" u32) (result bool)
    (canon lift (core func $main "run") (memory $memory "memory")
      (realloc (func $memory "realloc")) async (callback (func $main "callback")))))
