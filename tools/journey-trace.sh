#!/usr/bin/env bash
# Journey trace completeness, shared by every application journey.
#
#   trace_is_complete <trace-file> <expect-executor:true|false> <expect-statements>
#
# Returns 0 when the collected OTLP trace shows a complete request. Reads only
# its three arguments; touches no cluster; returns rather than exits.
#
# EVERYTHING HERE IS PLATFORM SHAPE EXCEPT ONE NUMBER. Authenticate, resolve,
# pull, compile, linker-setup, link, instantiate, and the callable-http and
# guest-sql authority acquisitions hold for ANY caller: they are what the host
# does to serve a routed request at all. The statement count is the route's
# OWN, which is why it is a parameter and not a literal -- a consumer whose
# route issues eight statements would otherwise fail inside an assertion whose
# name and message both sound like platform machinery ("trace is incomplete"),
# and would go looking in the wrong place.
#
# expect_executor distinguishes the two arms: a serving host that resolves an
# exact released wiring on demand acquires the executor-platform authority
# exactly once; one that does not must acquire it zero times. Both are asserted
# -- "zero times" is the half that catches an unintended acquisition.

trace_is_complete() {
  local trace_file=$1 expect_executor=$2 expect_statements=$3

  [[ -f $trace_file ]] || {
    echo "trace_is_complete: no trace at $trace_file" >&2
    return 1
  }
  case $expect_executor in
    true|false) ;;
    *) echo "trace_is_complete: expect_executor must be true or false, got '$expect_executor'" >&2
       return 1 ;;
  esac
  [[ $expect_statements =~ ^[0-9]+$ ]] || {
    echo "trace_is_complete: expect_statements must be a count, got '$expect_statements'" >&2
    return 1
  }

  jq -e --arg expect_executor "$expect_executor" \
    --argjson expect_statements "$expect_statements" '
    def spans: [.batches[]?.scopeSpans[]?.spans[]?];
    def attr($span; $key):
      ([$span.attributes[]? |
        select(.key == $key) | .value.stringValue][0] // "");
    spans as $spans |
    def count_named($name):
      [$spans[] | select(.name == $name)] | length;
    def count_acquired($class):
      [$spans[] |
        select(.name == "wamn.postgres.acquire" and
               attr(.; "wamn.authority_class") == $class)] | length;
    count_named("wamn.route.authenticate") == 1 and
    count_named("wamn.router.resolve") == 1 and
    count_named("wamn.component.pull") == 1 and
    count_named("wamn.component.compile") == 1 and
    count_named("wamn.component.linker_setup") == 1 and
    count_named("wamn.component.link") == 1 and
    count_named("wamn.component.instantiate") == 1 and
    count_acquired("callable-http") == 1 and
    count_acquired("guest-sql") == 1 and
    count_named("wamn.postgres") > 0 and
    count_named("wamn.postgres.statement") == $expect_statements and
    if $expect_executor == "true" then
      count_acquired("executor-platform") == 1
    else
      count_acquired("executor-platform") == 0
    end
  ' >/dev/null "$trace_file"
}
