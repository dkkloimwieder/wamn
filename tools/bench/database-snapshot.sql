-- Cluster-wide counters include platform background work in other databases.
SELECT jsonb_build_object(
    'at', clock_timestamp(),
    'wal', to_jsonb(w),
    'checkpointer', to_jsonb(c),
    'io', (SELECT jsonb_agg(to_jsonb(i)) FROM pg_stat_io i WHERE object = 'wal')
)::text
FROM pg_stat_wal w CROSS JOIN pg_stat_checkpointer c;
