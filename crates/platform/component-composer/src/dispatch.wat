;; Canonical ABI arena. Memory grows only for this request's arguments/results.
(func $realloc (export "cabi_realloc")
  (param $old i32) (param $length i32) (param $align i32) (param $size i32)
  (result i32) (local $ptr i32) (local $end i32) (local $pages i32)
  local.get $size i32.eqz if i32.const 0 return end
  global.get $heap local.get $align i32.const 1 i32.sub i32.add
  i32.const 0 local.get $align i32.sub i32.and local.tee $ptr
  local.get $size i32.add local.tee $end
  local.get $ptr i32.lt_u if unreachable end
  local.get $end i32.const 65535 i32.add i32.const 16 i32.shr_u local.set $pages
  local.get $pages memory.size i32.gt_u
  if local.get $pages memory.size i32.sub memory.grow i32.const -1 i32.eq
     if unreachable end end
  local.get $end global.set $heap
  local.get $ptr local.get $old
  local.get $length local.get $size local.get $length local.get $size i32.lt_u select
  memory.copy
  local.get $ptr)

(func $equal (param $a i32) (param $alen i32) (param $b i32) (param $blen i32)
  (result i32) (local $i i32)
  local.get $alen local.get $blen i32.ne if i32.const 0 return end
  block $done loop $next
    local.get $i local.get $alen i32.eq br_if $done
    local.get $a local.get $i i32.add i32.load8_u
    local.get $b local.get $i i32.add i32.load8_u i32.ne
    if i32.const 0 return end
    local.get $i i32.const 1 i32.add local.set $i br $next
  end end
  i32.const 1)
