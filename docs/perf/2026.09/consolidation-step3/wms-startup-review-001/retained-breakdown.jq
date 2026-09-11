      def spans: [.batches[]?.scopeSpans[]?.spans[]?];
      def attr($span; $key):
        ([$span.attributes[]? |
          select(.key == $key) | .value.stringValue][0] // "");
      def ms($span):
        (($span.endTimeUnixNano | tonumber) -
         ($span.startTimeUnixNano | tonumber)) / 1000000;
      spans as $spans |
      def named($name):
        ([$spans[] | select(.name == $name) | ms(.)] | add // 0);
      def acquired($class):
        ([$spans[] |
          select(.name == "wamn.postgres.acquire" and
                 attr(.; "wamn.authority_class") == $class) |
          ms(.)] | add // 0);
      {
        verdict: "pass",
        phase: $phase,
        http_total_ms: $http_total_ms,
        authentication_ms: named("wamn.route.authenticate"),
        resolution_ms: named("wamn.router.resolve"),
        artifact_pull_ms: named("wamn.component.pull"),
        compile_ms: named("wamn.component.compile"),
        linker_setup_ms: named("wamn.component.linker_setup"),
        link_ms: named("wamn.component.link"),
        instantiate_ms: named("wamn.component.instantiate"),
        executor_platform_acquire_ms: acquired("executor-platform"),
        callable_http_acquire_ms: acquired("callable-http"),
        guest_sql_acquire_ms: acquired("guest-sql"),
        sql_ms: named("wamn.postgres.statement"),
        guest_db_call_ms: named("wamn.postgres"),
        root_ms: named("handle_http_request"),
        real_work_ms:
          (named("wamn.postgres.statement") + named("wamn.component.instantiate")),
        overhead_ratio:
          (if (named("wamn.postgres.statement") +
               named("wamn.component.instantiate")) > 0
           then named("handle_http_request") /
                (named("wamn.postgres.statement") +
                 named("wamn.component.instantiate"))
           else null end)
      }
