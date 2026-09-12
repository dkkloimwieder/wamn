pub const CATALOG_SCHEMA_SQL: &str = concat!(
    include_str!("/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912/deploy/sql/catalog-schema-prefix.sql"),
    include_str!("/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912/deploy/sql/reject-immutable-row-change.sql"),
    include_str!("/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912/deploy/sql/catalog-schema.sql"),
);

pub const CONTROL_PORTABLE_STORE_SQL: &str = concat!(
    include_str!("/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912/deploy/sql/control-portable-store-prefix.sql"),
    include_str!("/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912/deploy/sql/reject-immutable-row-change.sql"),
    include_str!("/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912/deploy/sql/control-portable-store.sql"),
);

fn main() { let out = std::path::PathBuf::from(std::env::args_os().nth(1).unwrap()); std::fs::write(out.join("catalog-schema.sql"), CATALOG_SCHEMA_SQL).unwrap(); std::fs::write(out.join("control-portable-store.sql"), CONTROL_PORTABLE_STORE_SQL).unwrap(); }
