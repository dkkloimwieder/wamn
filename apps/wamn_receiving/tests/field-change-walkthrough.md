# Receiving field walkthrough

The three agreed commands passed on source `5b04da89900aee66fc6a09d45157876a369f4137`.
The run used the existing warm target in a disposable branch of `consolidation-sql-package-20260912`.
The original branch, `work/consolidation-approved-sql-integration-20260912`, remains preserved at `3034ba6efc9dedf2f642d662af79482447bf3f65`.
The starting tracked source was clean.

The Receiving README maps the application owners:

> Package `wamn_receiving` uses component `receiving`, data crate `wamn-receiving-data-access`, generated library `wamn-generated-receiving-tui`, UI crate `wamn-receiving-tui` with binary `wamn-receiving`, and test crate `wamn-receiving-tests`.

The disposable change adds nullable text `location.description` through migration `0002_location_description.sql`.
It updates the declared result, query, value conversion, and existing app-owned field expectations.
Generation emits the field in model and result descriptions, native and guest accessors, client descriptors, and read permissions.
The existing scalar test covers both a text description and `NULL`.

The database was fresh PostgreSQL 18.6 under `pg_virtualenv -t -v 18`.
Database creation, schema creation, and both migrations exited zero.
The private connection values below remain environment placeholders.

```bash
export RUSTC_WRAPPER=''
export CARGO_BUILD_JOBS=2
export CARGO_TARGET_DIR='/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912/target'
createdb receiving_walkthrough
psql -X -d receiving_walkthrough -v ON_ERROR_STOP=1 -c 'CREATE SCHEMA receiving'
psql -X -d receiving_walkthrough -v ON_ERROR_STOP=1 -f apps/wamn_receiving/migrations/0001_initial.sql
psql -X -d receiving_walkthrough -v ON_ERROR_STOP=1 -f apps/wamn_receiving/migrations/0002_location_description.sql
WAMN_SCHEMA_INTROSPECTION_PG_URL="$RECEIVING_DATABASE_URL" cargo +1.98.0 run --locked --offline -p wamn-schema-generator --example materialize_package -- write apps/wamn_receiving
(
  cd apps/wamn_receiving/tests
  CARGO_NET_OFFLINE=true cargo +1.98.0 sqlx prepare -D "$RECEIVING_SQLX_DATABASE_URL" -- --test receiving_sqlx_verifier --locked --offline
)
cargo +1.98.0 test --manifest-path apps/Cargo.toml --locked --offline -p wamn-receiving-data-access operation::tests::bounded_projection_rows_preserve_the_declared_wire_scalars -- --exact --nocapture
```

| Command | Exit | Seconds |
| --- | --- | --- |
| Generation | 0 | 14.707107 |
| App-local SQLx preparation | 0 | 166.749219 |
| Exact bounded-projection scalar case | 0 | 33.210751 |
| Complete PostgreSQL controller | 0 | 217.667777 |

The exact case output was:

```text
running 1 test
test operation::tests::bounded_projection_rows_preserve_the_declared_wire_scalars ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 37 filtered out; finished in 0.00s
```

The initial scope assessment failed because root `Cargo.lock` also changed its ordering.
Its recorded modification time falls within SQLx preparation, after generation finished.
No per-step lockfile snapshots were captured.
All 696 package records match by name, version, and source after sorting dependency lists.
Every other parsed TOML value also matches, so dependency resolution did not change.

Root committed that separate mechanical correction as `21b7ecf092011ed8edfe042b0a7286d772dd9d09` after the successful commands.
Only the failed scope assessment was repeated, against that corrected reference.
No generation, SQLx preparation, database setup, or test was repeated.
The original tested source remains `5b04da89900aee66fc6a09d45157876a369f4137`.
The initial failed scope and original passing command results remain preserved in the raw capture.

The final comparison contains 21 paths, all under `apps/wamn_receiving`.
Those paths comprise six authored files, thirteen generated files, and two SQLx metadata paths.
The old SQLx query metadata file was replaced by the metadata for the changed query.
No Acme files, shared tools, Cargo declarations, or guest dependency pins changed for the field.
The temporary PostgreSQL configuration directory was removed, and the field patch remains uncommitted.
Guest execution and rendered UI behavior remain outside this run.

The full app-only diff against `21b7ecf092011ed8edfe042b0a7286d772dd9d09` follows.

```diff
diff --git a/apps/wamn_receiving/data/src/operation.rs b/apps/wamn_receiving/data/src/operation.rs
index 25464c7f0..90dfb784e 100644
--- a/apps/wamn_receiving/data/src/operation.rs
+++ b/apps/wamn_receiving/data/src/operation.rs
@@ -434,6 +434,7 @@ struct RowsValue<T> {
 struct LocationValue {
     id: Box<str>,
     location_code: Box<str>,
+    description: Option<Box<str>>,
 }
 
 impl From<location_sql::ListLocationsRow> for LocationValue {
@@ -441,6 +442,7 @@ impl From<location_sql::ListLocationsRow> for LocationValue {
         Self {
             id: row.id.0.into_boxed_str(),
             location_code: row.location_code.into_boxed_str(),
+            description: row.description.map(String::into_boxed_str),
         }
     }
 }
@@ -1240,22 +1242,26 @@ mod tests {
 
     #[test]
     fn bounded_projection_rows_preserve_the_declared_wire_scalars() {
-        let location = LocationValue::from(location_sql::ListLocationsRow {
-            id: Uuid("00000000-0000-0000-0000-000000000001".to_owned()),
-            location_code: "DOCK-A".to_owned(),
-        });
-        assert_eq!(
-            serde_json::to_value(RowsValue {
-                rows: vec![location].into_boxed_slice()
-            })
-            .unwrap(),
-            serde_json::json!({
-                "rows": [{
-                    "id": "00000000-0000-0000-0000-000000000001",
-                    "location_code": "DOCK-A"
-                }]
-            })
-        );
+        for description in [Some("North loading dock"), None] {
+            let location = LocationValue::from(location_sql::ListLocationsRow {
+                id: Uuid("00000000-0000-0000-0000-000000000001".to_owned()),
+                location_code: "DOCK-A".to_owned(),
+                description: description.map(str::to_owned),
+            });
+            assert_eq!(
+                serde_json::to_value(RowsValue {
+                    rows: vec![location].into_boxed_slice()
+                })
+                .unwrap(),
+                serde_json::json!({
+                    "rows": [{
+                        "id": "00000000-0000-0000-0000-000000000001",
+                        "location_code": "DOCK-A",
+                        "description": description
+                    }]
+                })
+            );
+        }
 
         let screen = ReceiptScreenValue::from(screen_sql::LoadReceiptScreenRow {
             purchase_order_id: Uuid("00000000-0000-0000-0000-000000000002".to_owned()),
diff --git a/apps/wamn_receiving/generated/client/location.rs b/apps/wamn_receiving/generated/client/location.rs
index 09e7f6e92..a8793bcb9 100644
--- a/apps/wamn_receiving/generated/client/location.rs
+++ b/apps/wamn_receiving/generated/client/location.rs
@@ -6,6 +6,12 @@ use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};
 
 /// Every field the `location` model projects.
 pub const LOCATION_FIELDS: &[FieldDescriptor] = &[
+    FieldDescriptor {
+        path: "description",
+        type_name: "text",
+        nullable: true,
+        values: &[],
+    },
     FieldDescriptor {
         path: "id",
         type_name: "uuid",
@@ -31,6 +37,8 @@ pub struct LocationListRequest {
 /// Result of `wamn-receiving:location/list@1.0.0`.
 #[derive(Debug, Clone, PartialEq)]
 pub struct LocationListResult {
+    /// `text`
+    pub description: Option<String>,
     /// `uuid`
     pub id: uuid::Uuid,
     /// `text`
@@ -49,6 +57,12 @@ pub const LOCATION_LIST_INPUT: &[FieldDescriptor] = &[
 
 /// Result descriptors for `wamn-receiving:location/list@1.0.0`.
 pub const LOCATION_LIST_RESULT: &[FieldDescriptor] = &[
+    FieldDescriptor {
+        path: "description",
+        type_name: "text",
+        nullable: true,
+        values: &[],
+    },
     FieldDescriptor {
         path: "id",
         type_name: "uuid",
@@ -72,6 +86,10 @@ required: true, minimum: None, maximum: None, children: &[
 
 pub const LOCATION_LIST_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
 wamn_client::descriptor::FieldSchema {
+field: FieldDescriptor { path: "description", type_name: "text", nullable: true, values: &[] },
+required: true, minimum: None, maximum: None, children: &[
+], },
+wamn_client::descriptor::FieldSchema {
 field: FieldDescriptor { path: "id", type_name: "uuid", nullable: false, values: &[] },
 required: true, minimum: None, maximum: None, children: &[
 ], },
diff --git a/apps/wamn_receiving/generated/contracts/location/list.operation.json b/apps/wamn_receiving/generated/contracts/location/list.operation.json
index a414cc298..1c8bd06de 100644
--- a/apps/wamn_receiving/generated/contracts/location/list.operation.json
+++ b/apps/wamn_receiving/generated/contracts/location/list.operation.json
@@ -1 +1 @@
-{"connection":"postgres","grant":"wamn-receiving:location/list@1.0.0","kind":"projection","operation":"wamn-receiving:location/list@1.0.0","permission_token":"location.list","relations":[{"constraints":[],"insert_fields":[],"lock":false,"schema":"receiving","select_fields":["id","location_code"],"table":"location","update_fields":[]}],"result":"bounded_list","statements":[{"binds":[],"columns":[{"name":"id","nullable":false,"type":"uuid"},{"name":"location_code","nullable":false,"type":"text"}],"digest":"sha256:35923fd698b2742abe220aeb88e5318ddf8e9fd3adbb56a913d07dc01957e38a","name":"list_locations","path":"query/location.sql","transactional":false}],"visibility":"public"}
+{"connection":"postgres","grant":"wamn-receiving:location/list@1.0.0","kind":"projection","operation":"wamn-receiving:location/list@1.0.0","permission_token":"location.list","relations":[{"constraints":[],"insert_fields":[],"lock":false,"schema":"receiving","select_fields":["id","location_code","description"],"table":"location","update_fields":[]}],"result":"bounded_list","statements":[{"binds":[],"columns":[{"name":"id","nullable":false,"type":"uuid"},{"name":"location_code","nullable":false,"type":"text"},{"name":"description","nullable":true,"type":"text"}],"digest":"sha256:b5c60b008ae444cf0d5c8a33b1ad40e74c8eb313b4bf8cf623a3289eac59f55e","name":"list_locations","path":"query/location.sql","transactional":false}],"visibility":"public"}
diff --git a/apps/wamn_receiving/generated/contracts/location/list.result.json b/apps/wamn_receiving/generated/contracts/location/list.result.json
index b880a3f5c..b643a88f8 100644
--- a/apps/wamn_receiving/generated/contracts/location/list.result.json
+++ b/apps/wamn_receiving/generated/contracts/location/list.result.json
@@ -1 +1 @@
-{"class":"bounded_list","fields":[{"path":"id","type":"uuid","nullable":false,"values":[]},{"path":"location_code","type":"text","nullable":false,"values":[]}]}
\ No newline at end of file
+{"class":"bounded_list","fields":[{"path":"id","type":"uuid","nullable":false,"values":[]},{"path":"location_code","type":"text","nullable":false,"values":[]},{"path":"description","type":"text","nullable":true,"values":[]}]}
\ No newline at end of file
diff --git a/apps/wamn_receiving/generated/models/location.json b/apps/wamn_receiving/generated/models/location.json
index 6e3834a4f..3f576a109 100644
--- a/apps/wamn_receiving/generated/models/location.json
+++ b/apps/wamn_receiving/generated/models/location.json
@@ -1 +1 @@
-{"fields":[{"enum_values":null,"name":"id","nullable":false,"server_owned":true,"type":"uuid"},{"enum_values":null,"name":"location_code","nullable":false,"server_owned":false,"type":"text"}],"model":"location","owner":"wamn_receiving","schema":"receiving","table":"location"}
\ No newline at end of file
+{"fields":[{"enum_values":null,"name":"description","nullable":true,"server_owned":false,"type":"text"},{"enum_values":null,"name":"id","nullable":false,"server_owned":true,"type":"uuid"},{"enum_values":null,"name":"location_code","nullable":false,"server_owned":false,"type":"text"}],"model":"location","owner":"wamn_receiving","schema":"receiving","table":"location"}
\ No newline at end of file
diff --git a/apps/wamn_receiving/generated/native-verifier/location.rs b/apps/wamn_receiving/generated/native-verifier/location.rs
index ab8df3636..2bb9a66f5 100644
--- a/apps/wamn_receiving/generated/native-verifier/location.rs
+++ b/apps/wamn_receiving/generated/native-verifier/location.rs
@@ -2,6 +2,7 @@
 
 #[derive(Debug, sqlx::FromRow)]
 pub struct LocationRow {
+    pub description: Option<String>,
     pub id: uuid::Uuid,
     pub location_code: String,
 }
diff --git a/apps/wamn_receiving/generated/native-verifier/location_list.rs b/apps/wamn_receiving/generated/native-verifier/location_list.rs
index 2db1c6e7b..032e891b2 100644
--- a/apps/wamn_receiving/generated/native-verifier/location_list.rs
+++ b/apps/wamn_receiving/generated/native-verifier/location_list.rs
@@ -4,6 +4,7 @@
 pub(crate) struct ListLocationsRow {
     pub id: uuid::Uuid,
     pub location_code: String,
+    pub description: Option<String>,
 }
 
 pub(crate) const LIST_LOCATIONS_SQL: &str = include_str!("../../query/location.sql");
diff --git a/apps/wamn_receiving/generated/package-weld.json b/apps/wamn_receiving/generated/package-weld.json
index ccb6f0f7a..33db52aed 100644
--- a/apps/wamn_receiving/generated/package-weld.json
+++ b/apps/wamn_receiving/generated/package-weld.json
@@ -1 +1 @@
-{"application_sql_corpus_identity":"sha256:d826dbbb091eef13c1935ce6e4a1fcf0fecddb71fdb844302a6bed0abd0a0287","promotion_state":"eligible","provenance":{"generator":"wamn-schema-generator/0.1.0","toolchain":"rust-1.98.0"},"required_platform_policy_contract":{"id":"receiving_data_access","state":"satisfied"},"required_schema_contract":{"tables":[{"constraints":[{"definition":{"columns":["id"],"kind":"primary_key"},"name":"item_id_pkey"},{"definition":{"columns":["item_number"],"kind":"unique"},"name":"item_item_number_key"}],"fields":[{"name":"id","nullable":false,"type":"uuid"},{"name":"item_number","nullable":false,"type":"text"}],"schema":"receiving","table":"item"},{"constraints":[{"definition":{"columns":["id"],"kind":"primary_key"},"name":"location_id_pkey"},{"definition":{"columns":["location_code"],"kind":"unique"},"name":"location_location_code_key"}],"fields":[{"name":"id","nullable":false,"type":"uuid"},{"name":"location_code","nullable":false,"type":"text"}],"schema":"receiving","table":"location"},{"constraints":[{"definition":{"columns":["id"],"kind":"primary_key"},"name":"purchase_order_id_pkey"},{"definition":{"columns":["purchase_order_number"],"kind":"unique"},"name":"purchase_order_purchase_order_number_key"},{"definition":{"expression":"(status = ANY (ARRAY['open'::text, 'complete'::text, 'cancelled'::text]))","kind":"check"},"name":"purchase_order_status_check"}],"fields":[{"name":"created_at","nullable":false,"type":"timestamptz"},{"name":"id","nullable":false,"type":"uuid"},{"name":"purchase_order_number","nullable":false,"type":"text"},{"name":"row_version","nullable":false,"type":"int64"},{"name":"status","nullable":false,"type":"text"},{"name":"supplier_id","nullable":false,"type":"uuid"},{"name":"updated_at","nullable":false,"type":"timestamptz"}],"schema":"receiving","table":"purchase_order"},{"constraints":[{"definition":{"columns":["id"],"kind":"primary_key"},"name":"purchase_order_line_id_pkey"},{"definition":{"columns":[{"column":"item_id","referenced_column":"id"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"item"},"name":"purchase_order_line_item_id_fkey"},{"definition":{"expression":"(ordered_quantity > (0)::numeric)","kind":"check"},"name":"purchase_order_line_ordered_quantity_check"},{"definition":{"expression":"((received_quantity >= (0)::numeric) AND (received_quantity <= ordered_quantity))","kind":"check"},"name":"purchase_order_line_ordered_quantity_received_quantity_check"},{"definition":{"columns":[{"column":"purchase_order_id","referenced_column":"id"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"purchase_order"},"name":"purchase_order_line_purchase_order_id_fkey"},{"definition":{"columns":["purchase_order_id","line_number"],"kind":"unique"},"name":"purchase_order_line_purchase_order_id_line_number_key"}],"fields":[{"name":"id","nullable":false,"type":"uuid"},{"name":"item_id","nullable":false,"type":"uuid"},{"name":"line_number","nullable":false,"type":"int32"},{"name":"ordered_quantity","nullable":false,"type":"numeric"},{"name":"purchase_order_id","nullable":false,"type":"uuid"},{"name":"received_quantity","nullable":false,"type":"numeric"}],"schema":"receiving","table":"purchase_order_line"},{"constraints":[{"definition":{"columns":["id"],"kind":"primary_key"},"name":"receipt_id_pkey"},{"definition":{"columns":[{"column":"idempotency_key","referenced_column":"idempotency_key"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"record_receipt_command"},"name":"receipt_idempotency_key_fkey"},{"definition":{"columns":["idempotency_key"],"kind":"unique"},"name":"receipt_idempotency_key_key"},{"definition":{"columns":[{"column":"purchase_order_id","referenced_column":"id"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"purchase_order"},"name":"receipt_purchase_order_id_fkey"},{"definition":{"columns":["purchase_order_id","receipt_reference"],"kind":"unique"},"name":"receipt_purchase_order_id_receipt_reference_key"}],"fields":[{"name":"created_at","nullable":false,"type":"timestamptz"},{"name":"id","nullable":false,"type":"uuid"},{"name":"idempotency_key","nullable":false,"type":"text"},{"name":"occurred_at","nullable":false,"type":"timestamptz"},{"name":"purchase_order_id","nullable":false,"type":"uuid"},{"name":"receipt_reference","nullable":false,"type":"text"}],"schema":"receiving","table":"receipt"},{"constraints":[{"definition":{"columns":["id"],"kind":"primary_key"},"name":"receipt_line_id_pkey"},{"definition":{"columns":[{"column":"location_id","referenced_column":"id"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"location"},"name":"receipt_line_location_id_fkey"},{"definition":{"columns":[{"column":"purchase_order_line_id","referenced_column":"id"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"purchase_order_line"},"name":"receipt_line_purchase_order_line_id_fkey"},{"definition":{"expression":"(quantity > (0)::numeric)","kind":"check"},"name":"receipt_line_quantity_check"},{"definition":{"columns":[{"column":"receipt_id","referenced_column":"id"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"receipt"},"name":"receipt_line_receipt_id_fkey"}],"fields":[{"name":"id","nullable":false,"type":"uuid"},{"name":"location_id","nullable":false,"type":"uuid"},{"name":"purchase_order_line_id","nullable":false,"type":"uuid"},{"name":"quantity","nullable":false,"type":"numeric"},{"name":"receipt_id","nullable":false,"type":"uuid"}],"schema":"receiving","table":"receipt_line"},{"constraints":[{"definition":{"columns":["idempotency_key"],"kind":"primary_key"},"name":"record_receipt_command_idempotency_key_pkey"}],"fields":[{"name":"canonical_command","nullable":false,"type":"bytes"},{"name":"idempotency_key","nullable":false,"type":"text"},{"name":"purchase_order_id","nullable":false,"type":"uuid"},{"name":"purchase_order_status","nullable":true,"type":"text"},{"name":"receipt_id","nullable":false,"type":"uuid"},{"name":"row_version","nullable":true,"type":"int64"}],"schema":"receiving","table":"record_receipt_command"}]},"verified_schema_state_id":"sha256:8127f449e48e5a608324c7c3da0446ae1c7ff6520be2527ac3c7d2c4bd0f04ec"}
\ No newline at end of file
+{"application_sql_corpus_identity":"sha256:a63f50771a365ced72387519ade6612947c31d200a689c83412c79ea3677997e","promotion_state":"eligible","provenance":{"generator":"wamn-schema-generator/0.1.0","toolchain":"rust-1.98.0"},"required_platform_policy_contract":{"id":"receiving_data_access","state":"satisfied"},"required_schema_contract":{"tables":[{"constraints":[{"definition":{"columns":["id"],"kind":"primary_key"},"name":"item_id_pkey"},{"definition":{"columns":["item_number"],"kind":"unique"},"name":"item_item_number_key"}],"fields":[{"name":"id","nullable":false,"type":"uuid"},{"name":"item_number","nullable":false,"type":"text"}],"schema":"receiving","table":"item"},{"constraints":[{"definition":{"columns":["id"],"kind":"primary_key"},"name":"location_id_pkey"},{"definition":{"columns":["location_code"],"kind":"unique"},"name":"location_location_code_key"}],"fields":[{"name":"description","nullable":true,"type":"text"},{"name":"id","nullable":false,"type":"uuid"},{"name":"location_code","nullable":false,"type":"text"}],"schema":"receiving","table":"location"},{"constraints":[{"definition":{"columns":["id"],"kind":"primary_key"},"name":"purchase_order_id_pkey"},{"definition":{"columns":["purchase_order_number"],"kind":"unique"},"name":"purchase_order_purchase_order_number_key"},{"definition":{"expression":"(status = ANY (ARRAY['open'::text, 'complete'::text, 'cancelled'::text]))","kind":"check"},"name":"purchase_order_status_check"}],"fields":[{"name":"created_at","nullable":false,"type":"timestamptz"},{"name":"id","nullable":false,"type":"uuid"},{"name":"purchase_order_number","nullable":false,"type":"text"},{"name":"row_version","nullable":false,"type":"int64"},{"name":"status","nullable":false,"type":"text"},{"name":"supplier_id","nullable":false,"type":"uuid"},{"name":"updated_at","nullable":false,"type":"timestamptz"}],"schema":"receiving","table":"purchase_order"},{"constraints":[{"definition":{"columns":["id"],"kind":"primary_key"},"name":"purchase_order_line_id_pkey"},{"definition":{"columns":[{"column":"item_id","referenced_column":"id"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"item"},"name":"purchase_order_line_item_id_fkey"},{"definition":{"expression":"(ordered_quantity > (0)::numeric)","kind":"check"},"name":"purchase_order_line_ordered_quantity_check"},{"definition":{"expression":"((received_quantity >= (0)::numeric) AND (received_quantity <= ordered_quantity))","kind":"check"},"name":"purchase_order_line_ordered_quantity_received_quantity_check"},{"definition":{"columns":[{"column":"purchase_order_id","referenced_column":"id"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"purchase_order"},"name":"purchase_order_line_purchase_order_id_fkey"},{"definition":{"columns":["purchase_order_id","line_number"],"kind":"unique"},"name":"purchase_order_line_purchase_order_id_line_number_key"}],"fields":[{"name":"id","nullable":false,"type":"uuid"},{"name":"item_id","nullable":false,"type":"uuid"},{"name":"line_number","nullable":false,"type":"int32"},{"name":"ordered_quantity","nullable":false,"type":"numeric"},{"name":"purchase_order_id","nullable":false,"type":"uuid"},{"name":"received_quantity","nullable":false,"type":"numeric"}],"schema":"receiving","table":"purchase_order_line"},{"constraints":[{"definition":{"columns":["id"],"kind":"primary_key"},"name":"receipt_id_pkey"},{"definition":{"columns":[{"column":"idempotency_key","referenced_column":"idempotency_key"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"record_receipt_command"},"name":"receipt_idempotency_key_fkey"},{"definition":{"columns":["idempotency_key"],"kind":"unique"},"name":"receipt_idempotency_key_key"},{"definition":{"columns":[{"column":"purchase_order_id","referenced_column":"id"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"purchase_order"},"name":"receipt_purchase_order_id_fkey"},{"definition":{"columns":["purchase_order_id","receipt_reference"],"kind":"unique"},"name":"receipt_purchase_order_id_receipt_reference_key"}],"fields":[{"name":"created_at","nullable":false,"type":"timestamptz"},{"name":"id","nullable":false,"type":"uuid"},{"name":"idempotency_key","nullable":false,"type":"text"},{"name":"occurred_at","nullable":false,"type":"timestamptz"},{"name":"purchase_order_id","nullable":false,"type":"uuid"},{"name":"receipt_reference","nullable":false,"type":"text"}],"schema":"receiving","table":"receipt"},{"constraints":[{"definition":{"columns":["id"],"kind":"primary_key"},"name":"receipt_line_id_pkey"},{"definition":{"columns":[{"column":"location_id","referenced_column":"id"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"location"},"name":"receipt_line_location_id_fkey"},{"definition":{"columns":[{"column":"purchase_order_line_id","referenced_column":"id"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"purchase_order_line"},"name":"receipt_line_purchase_order_line_id_fkey"},{"definition":{"expression":"(quantity > (0)::numeric)","kind":"check"},"name":"receipt_line_quantity_check"},{"definition":{"columns":[{"column":"receipt_id","referenced_column":"id"}],"kind":"foreign_key","on_delete":"no_action","on_update":"no_action","referenced_schema":"receiving","referenced_table":"receipt"},"name":"receipt_line_receipt_id_fkey"}],"fields":[{"name":"id","nullable":false,"type":"uuid"},{"name":"location_id","nullable":false,"type":"uuid"},{"name":"purchase_order_line_id","nullable":false,"type":"uuid"},{"name":"quantity","nullable":false,"type":"numeric"},{"name":"receipt_id","nullable":false,"type":"uuid"}],"schema":"receiving","table":"receipt_line"},{"constraints":[{"definition":{"columns":["idempotency_key"],"kind":"primary_key"},"name":"record_receipt_command_idempotency_key_pkey"}],"fields":[{"name":"canonical_command","nullable":false,"type":"bytes"},{"name":"idempotency_key","nullable":false,"type":"text"},{"name":"purchase_order_id","nullable":false,"type":"uuid"},{"name":"purchase_order_status","nullable":true,"type":"text"},{"name":"receipt_id","nullable":false,"type":"uuid"},{"name":"row_version","nullable":true,"type":"int64"}],"schema":"receiving","table":"record_receipt_command"}]},"verified_schema_state_id":"sha256:82c361643353df462883a176abbd2b841afb9d1017192146460df34fbf717713"}
\ No newline at end of file
diff --git a/apps/wamn_receiving/generated/parity/location.json b/apps/wamn_receiving/generated/parity/location.json
index 93f0bfb5a..4edb738ab 100644
--- a/apps/wamn_receiving/generated/parity/location.json
+++ b/apps/wamn_receiving/generated/parity/location.json
@@ -1 +1 @@
-{"accessor_binds":[],"fields":[{"field":"id","native_rust":"uuid::Uuid","nullable":false,"postgres":"uuid","wamn_rust":"wamn_postgres_statements::Uuid","wamn_sql_value":"uuid"},{"field":"location_code","native_rust":"String","nullable":false,"postgres":"text","wamn_rust":"String","wamn_sql_value":"text"}],"model":"location","rule":"same_sql_file_two_projection_structs"}
\ No newline at end of file
+{"accessor_binds":[],"fields":[{"field":"description","native_rust":"Option<String>","nullable":true,"postgres":"text","wamn_rust":"Option<String>","wamn_sql_value":"text"},{"field":"id","native_rust":"uuid::Uuid","nullable":false,"postgres":"uuid","wamn_rust":"wamn_postgres_statements::Uuid","wamn_sql_value":"uuid"},{"field":"location_code","native_rust":"String","nullable":false,"postgres":"text","wamn_rust":"String","wamn_sql_value":"text"}],"model":"location","rule":"same_sql_file_two_projection_structs"}
\ No newline at end of file
diff --git a/apps/wamn_receiving/generated/parity/location_list.json b/apps/wamn_receiving/generated/parity/location_list.json
index 690d48adf..026a634f7 100644
--- a/apps/wamn_receiving/generated/parity/location_list.json
+++ b/apps/wamn_receiving/generated/parity/location_list.json
@@ -1 +1 @@
-{"accessor_binds":[],"fields":[{"field":"list_locations.id","native_rust":"uuid::Uuid","nullable":false,"postgres":"uuid","wamn_rust":"wamn_postgres_statements::Uuid","wamn_sql_value":"uuid"},{"field":"list_locations.location_code","native_rust":"String","nullable":false,"postgres":"text","wamn_rust":"String","wamn_sql_value":"text"}],"model":"location_list","rule":"same_sql_file_two_projection_structs"}
\ No newline at end of file
+{"accessor_binds":[],"fields":[{"field":"list_locations.id","native_rust":"uuid::Uuid","nullable":false,"postgres":"uuid","wamn_rust":"wamn_postgres_statements::Uuid","wamn_sql_value":"uuid"},{"field":"list_locations.location_code","native_rust":"String","nullable":false,"postgres":"text","wamn_rust":"String","wamn_sql_value":"text"},{"field":"list_locations.description","native_rust":"Option<String>","nullable":true,"postgres":"text","wamn_rust":"Option<String>","wamn_sql_value":"text"}],"model":"location_list","rule":"same_sql_file_two_projection_structs"}
\ No newline at end of file
diff --git a/apps/wamn_receiving/generated/platform-policy/data-access.json b/apps/wamn_receiving/generated/platform-policy/data-access.json
index f1eb054d3..61c84646a 100644
--- a/apps/wamn_receiving/generated/platform-policy/data-access.json
+++ b/apps/wamn_receiving/generated/platform-policy/data-access.json
@@ -1 +1 @@
-{"contract":"receiving_data_access","manifest_sha256":"sha256:b8899211b46b05a2eb6ee59c6dee8cd816763b1914c6b36c5435e16158136d6b","package":"wamn_receiving@1.0.0","relations":[{"all_fields":["id","item_number"],"insert_fields":[],"lock":false,"schema":"receiving","select_fields":["id","item_number"],"table":"item","update_fields":[]},{"all_fields":["id","location_code"],"insert_fields":[],"lock":true,"lock_update_field":"id","schema":"receiving","select_fields":["id","location_code"],"table":"location","update_fields":[]},{"all_fields":["created_at","id","purchase_order_number","row_version","status","supplier_id","updated_at"],"insert_fields":[],"lock":true,"lock_update_field":"row_version","schema":"receiving","select_fields":["created_at","id","purchase_order_number","row_version","status","supplier_id","updated_at"],"table":"purchase_order","update_fields":["row_version","status","supplier_id","updated_at"]},{"all_fields":["id","item_id","line_number","ordered_quantity","purchase_order_id","received_quantity"],"insert_fields":[],"lock":true,"lock_update_field":"received_quantity","schema":"receiving","select_fields":["id","item_id","line_number","ordered_quantity","purchase_order_id","received_quantity"],"table":"purchase_order_line","update_fields":["received_quantity"]},{"all_fields":["created_at","id","idempotency_key","occurred_at","purchase_order_id","receipt_reference"],"insert_fields":["id","idempotency_key","occurred_at","purchase_order_id","receipt_reference"],"lock":false,"schema":"receiving","select_fields":["created_at","id","idempotency_key","occurred_at","purchase_order_id","receipt_reference"],"table":"receipt","update_fields":[]},{"all_fields":["id","location_id","purchase_order_line_id","quantity","receipt_id"],"insert_fields":["location_id","purchase_order_line_id","quantity","receipt_id"],"lock":false,"schema":"receiving","select_fields":["id"],"table":"receipt_line","update_fields":[]},{"all_fields":["canonical_command","idempotency_key","purchase_order_id","purchase_order_status","receipt_id","row_version"],"insert_fields":["canonical_command","idempotency_key","purchase_order_id"],"lock":false,"schema":"receiving","select_fields":["canonical_command","idempotency_key","purchase_order_id","purchase_order_status","receipt_id","row_version"],"table":"record_receipt_command","update_fields":["purchase_order_status","row_version"]}],"role":"wamn_app","schemas":["receiving"]}
\ No newline at end of file
+{"contract":"receiving_data_access","manifest_sha256":"sha256:0dd1b1aa97ef75fcf11b36a0f4647203827321b6be86fc86e5a2b0f668fdef34","package":"wamn_receiving@1.0.0","relations":[{"all_fields":["id","item_number"],"insert_fields":[],"lock":false,"schema":"receiving","select_fields":["id","item_number"],"table":"item","update_fields":[]},{"all_fields":["description","id","location_code"],"insert_fields":[],"lock":true,"lock_update_field":"description","schema":"receiving","select_fields":["description","id","location_code"],"table":"location","update_fields":[]},{"all_fields":["created_at","id","purchase_order_number","row_version","status","supplier_id","updated_at"],"insert_fields":[],"lock":true,"lock_update_field":"row_version","schema":"receiving","select_fields":["created_at","id","purchase_order_number","row_version","status","supplier_id","updated_at"],"table":"purchase_order","update_fields":["row_version","status","supplier_id","updated_at"]},{"all_fields":["id","item_id","line_number","ordered_quantity","purchase_order_id","received_quantity"],"insert_fields":[],"lock":true,"lock_update_field":"received_quantity","schema":"receiving","select_fields":["id","item_id","line_number","ordered_quantity","purchase_order_id","received_quantity"],"table":"purchase_order_line","update_fields":["received_quantity"]},{"all_fields":["created_at","id","idempotency_key","occurred_at","purchase_order_id","receipt_reference"],"insert_fields":["id","idempotency_key","occurred_at","purchase_order_id","receipt_reference"],"lock":false,"schema":"receiving","select_fields":["created_at","id","idempotency_key","occurred_at","purchase_order_id","receipt_reference"],"table":"receipt","update_fields":[]},{"all_fields":["id","location_id","purchase_order_line_id","quantity","receipt_id"],"insert_fields":["location_id","purchase_order_line_id","quantity","receipt_id"],"lock":false,"schema":"receiving","select_fields":["id"],"table":"receipt_line","update_fields":[]},{"all_fields":["canonical_command","idempotency_key","purchase_order_id","purchase_order_status","receipt_id","row_version"],"insert_fields":["canonical_command","idempotency_key","purchase_order_id"],"lock":false,"schema":"receiving","select_fields":["canonical_command","idempotency_key","purchase_order_id","purchase_order_status","receipt_id","row_version"],"table":"record_receipt_command","update_fields":["purchase_order_status","row_version"]}],"role":"wamn_app","schemas":["receiving"]}
\ No newline at end of file
diff --git a/apps/wamn_receiving/generated/source-map/location_list.json b/apps/wamn_receiving/generated/source-map/location_list.json
index a92176c1c..46b72eabc 100644
--- a/apps/wamn_receiving/generated/source-map/location_list.json
+++ b/apps/wamn_receiving/generated/source-map/location_list.json
@@ -1 +1 @@
-{"kind":"projection","manifest":"wamn.json#/custom_operations/location.list","native_bind_fixtures":[],"native_rows":[{"fields":[{"name":"id","type":"uuid::Uuid"},{"name":"location_code","type":"String"}],"name":"ListLocationsRow","visibility":"crate"}],"operation":"location.list","relations":[{"constraints":[],"insert_fields":[],"lock":false,"schema":"receiving","select_fields":["id","location_code"],"table":"location","update_fields":[]}],"statements":{"list_locations":{"fetch":"bounded_list","parameters":[],"path":"query/location.sql","row":[{"name":"id","nullable":false,"type":"uuid"},{"name":"location_code","nullable":false,"type":"text"}]}},"wamn_accessors":[{"binds":[],"fetch":"bounded_list","name":"list_locations","row":"ListLocationsRow","statement_digest_constant":"LIST_LOCATIONS_DIGEST"}],"wamn_rows":[{"fields":[{"name":"id","type":"wamn_postgres_statements::Uuid"},{"name":"location_code","type":"String"}],"name":"ListLocationsRow","visibility":"crate"}]}
\ No newline at end of file
+{"kind":"projection","manifest":"wamn.json#/custom_operations/location.list","native_bind_fixtures":[],"native_rows":[{"fields":[{"name":"id","type":"uuid::Uuid"},{"name":"location_code","type":"String"},{"name":"description","type":"Option<String>"}],"name":"ListLocationsRow","visibility":"crate"}],"operation":"location.list","relations":[{"constraints":[],"insert_fields":[],"lock":false,"schema":"receiving","select_fields":["id","location_code","description"],"table":"location","update_fields":[]}],"statements":{"list_locations":{"fetch":"bounded_list","parameters":[],"path":"query/location.sql","row":[{"name":"id","nullable":false,"type":"uuid"},{"name":"location_code","nullable":false,"type":"text"},{"name":"description","nullable":true,"type":"text"}]}},"wamn_accessors":[{"binds":[],"fetch":"bounded_list","name":"list_locations","row":"ListLocationsRow","statement_digest_constant":"LIST_LOCATIONS_DIGEST"}],"wamn_rows":[{"fields":[{"name":"id","type":"wamn_postgres_statements::Uuid"},{"name":"location_code","type":"String"},{"name":"description","type":"Option<String>"}],"name":"ListLocationsRow","visibility":"crate"}]}
\ No newline at end of file
diff --git a/apps/wamn_receiving/generated/wamn/location.rs b/apps/wamn_receiving/generated/wamn/location.rs
index 3d27baa2d..cd2eacad8 100644
--- a/apps/wamn_receiving/generated/wamn/location.rs
+++ b/apps/wamn_receiving/generated/wamn/location.rs
@@ -4,6 +4,7 @@ use wamn_postgres_statements::Connection;
 
 #[derive(Debug)]
 pub struct LocationRow {
+    pub description: Option<String>,
     pub id: wamn_postgres_statements::Uuid,
     pub location_code: String,
 }
diff --git a/apps/wamn_receiving/generated/wamn/location_list.rs b/apps/wamn_receiving/generated/wamn/location_list.rs
index 714d0fb2b..1e96bb068 100644
--- a/apps/wamn_receiving/generated/wamn/location_list.rs
+++ b/apps/wamn_receiving/generated/wamn/location_list.rs
@@ -6,9 +6,10 @@ use wamn_postgres_statements::Transaction;
 pub(crate) struct ListLocationsRow {
     pub id: wamn_postgres_statements::Uuid,
     pub location_code: String,
+    pub description: Option<String>,
 }
 
-pub(crate) const LIST_LOCATIONS_DIGEST: &str = "sha256:35923fd698b2742abe220aeb88e5318ddf8e9fd3adbb56a913d07dc01957e38a";
+pub(crate) const LIST_LOCATIONS_DIGEST: &str = "sha256:b5c60b008ae444cf0d5c8a33b1ad40e74c8eb313b4bf8cf623a3289eac59f55e";
 
 pub(crate) async fn list_locations(
     transaction: &mut Transaction,
@@ -19,6 +20,7 @@ pub(crate) async fn list_locations(
         Ok(ListLocationsRow {
             id: row.decode("id")?,
             location_code: row.decode("location_code")?,
+            description: row.decode("description")?,
         })
     })
 }
diff --git a/apps/wamn_receiving/query/location.sql b/apps/wamn_receiving/query/location.sql
index 9f09470cd..1ba8b1865 100644
--- a/apps/wamn_receiving/query/location.sql
+++ b/apps/wamn_receiving/query/location.sql
@@ -1,6 +1,7 @@
 SELECT
     location.id,
-    location.location_code
+    location.location_code,
+    location.description
 FROM location AS location
 ORDER BY
     location.location_code ASC,
diff --git a/apps/wamn_receiving/tests/.sqlx/query-35923fd698b2742abe220aeb88e5318ddf8e9fd3adbb56a913d07dc01957e38a.json b/apps/wamn_receiving/tests/.sqlx/query-35923fd698b2742abe220aeb88e5318ddf8e9fd3adbb56a913d07dc01957e38a.json
deleted file mode 100644
index fe2caeb1e..000000000
--- a/apps/wamn_receiving/tests/.sqlx/query-35923fd698b2742abe220aeb88e5318ddf8e9fd3adbb56a913d07dc01957e38a.json
+++ /dev/null
@@ -1,38 +0,0 @@
-{
-  "db_name": "PostgreSQL",
-  "query": "SELECT\n    location.id,\n    location.location_code\nFROM location AS location\nORDER BY\n    location.location_code ASC,\n    location.id ASC;\n",
-  "describe": {
-    "columns": [
-      {
-        "ordinal": 0,
-        "name": "id",
-        "type_info": "Uuid",
-        "origin": {
-          "Table": {
-            "table": "location",
-            "name": "id"
-          }
-        }
-      },
-      {
-        "ordinal": 1,
-        "name": "location_code",
-        "type_info": "Text",
-        "origin": {
-          "Table": {
-            "table": "location",
-            "name": "location_code"
-          }
-        }
-      }
-    ],
-    "parameters": {
-      "Left": []
-    },
-    "nullable": [
-      false,
-      false
-    ]
-  },
-  "hash": "35923fd698b2742abe220aeb88e5318ddf8e9fd3adbb56a913d07dc01957e38a"
-}
diff --git a/apps/wamn_receiving/tests/generation.rs b/apps/wamn_receiving/tests/generation.rs
index f10613f20..d20fdbac2 100644
--- a/apps/wamn_receiving/tests/generation.rs
+++ b/apps/wamn_receiving/tests/generation.rs
@@ -146,6 +146,7 @@ fn receiving_catalog() -> CatalogIr {
                 None,
             ),
             Column::new("location_code", ColumnType::Text, false, None, None),
+            Column::new("description", ColumnType::Text, true, None, None),
         ],
         vec![Constraint::primary_key("location_id_pkey", ["id"]).unwrap()],
         Vec::new(),
@@ -1130,10 +1131,13 @@ fn shipped_receiving_manifest_and_authored_corpus_generate_without_drift() {
         .iter()
         .find(|relation| relation["table"] == "location")
         .unwrap();
-    // `location.list` reads the code, so the derived ACL grants SELECT on it.
+    // `location.list` reads the code and description, so their ACL grants SELECT.
     // The invariant this pins is that location is never WRITTEN: a read
     // operation may widen the select set and must not touch the rest.
-    assert_eq!(location["select_fields"], json!(["id", "location_code"]));
+    assert_eq!(
+        location["select_fields"],
+        json!(["description", "id", "location_code"])
+    );
     assert_eq!(location["insert_fields"], json!([]));
     assert_eq!(location["update_fields"], json!([]));
     assert_eq!(location["lock"], true);
diff --git a/apps/wamn_receiving/tests/operator_pty.py b/apps/wamn_receiving/tests/operator_pty.py
index 39197d9c5..2d73b4cb2 100755
--- a/apps/wamn_receiving/tests/operator_pty.py
+++ b/apps/wamn_receiving/tests/operator_pty.py
@@ -40,7 +40,7 @@ def check_descriptors(root):
     result = json.loads((contracts / "list.result.json").read_text())
     require(operation["result"] == result["class"] == "bounded_list", "location.list cardinality changed; update the fixture")
     fields = {(field["path"], field["type"], field["nullable"]) for field in result["fields"]}
-    require(fields == {("id", "uuid", False), ("location_code", "text", False)}, "location.list result descriptors changed; update the fixture")
+    require(fields == {("id", "uuid", False), ("location_code", "text", False), ("description", "text", True)}, "location.list result descriptors changed; update the fixture")
     attachments = json.loads((package / "publication/attachments.json").read_text())
     attachment = attachments["location-list-http"]
     require(attachment["registered-operation"] == operation["operation"], "location.list attachment changed")
diff --git a/apps/wamn_receiving/wamn.json b/apps/wamn_receiving/wamn.json
index fe7121b00..d13e6e33d 100644
--- a/apps/wamn_receiving/wamn.json
+++ b/apps/wamn_receiving/wamn.json
@@ -1165,6 +1165,11 @@
             "path": "location_code",
             "type": "text",
             "nullable": false
+          },
+          {
+            "path": "description",
+            "type": "text",
+            "nullable": true
           }
         ]
       },
@@ -1197,7 +1202,8 @@
           "table": "location",
           "select_fields": [
             "id",
-            "location_code"
+            "location_code",
+            "description"
           ],
           "insert_fields": [],
           "update_fields": [],
@@ -1220,6 +1226,11 @@
               "name": "location_code",
               "type": "text",
               "nullable": false
+            },
+            {
+              "name": "description",
+              "type": "text",
+              "nullable": true
             }
           ]
         }
diff --git a/apps/wamn_receiving/migrations/0002_location_description.sql b/apps/wamn_receiving/migrations/0002_location_description.sql
new file mode 100644
index 000000000..7a4ff25f5
--- /dev/null
+++ b/apps/wamn_receiving/migrations/0002_location_description.sql
@@ -0,0 +1 @@
+ALTER TABLE receiving.location ADD COLUMN description text;
diff --git a/apps/wamn_receiving/tests/.sqlx/query-b5c60b008ae444cf0d5c8a33b1ad40e74c8eb313b4bf8cf623a3289eac59f55e.json b/apps/wamn_receiving/tests/.sqlx/query-b5c60b008ae444cf0d5c8a33b1ad40e74c8eb313b4bf8cf623a3289eac59f55e.json
new file mode 100644
index 000000000..24dc8da8c
--- /dev/null
+++ b/apps/wamn_receiving/tests/.sqlx/query-b5c60b008ae444cf0d5c8a33b1ad40e74c8eb313b4bf8cf623a3289eac59f55e.json
@@ -0,0 +1,50 @@
+{
+  "db_name": "PostgreSQL",
+  "query": "SELECT\n    location.id,\n    location.location_code,\n    location.description\nFROM location AS location\nORDER BY\n    location.location_code ASC,\n    location.id ASC;\n",
+  "describe": {
+    "columns": [
+      {
+        "ordinal": 0,
+        "name": "id",
+        "type_info": "Uuid",
+        "origin": {
+          "Table": {
+            "table": "location",
+            "name": "id"
+          }
+        }
+      },
+      {
+        "ordinal": 1,
+        "name": "location_code",
+        "type_info": "Text",
+        "origin": {
+          "Table": {
+            "table": "location",
+            "name": "location_code"
+          }
+        }
+      },
+      {
+        "ordinal": 2,
+        "name": "description",
+        "type_info": "Text",
+        "origin": {
+          "Table": {
+            "table": "location",
+            "name": "description"
+          }
+        }
+      }
+    ],
+    "parameters": {
+      "Left": []
+    },
+    "nullable": [
+      false,
+      false,
+      true
+    ]
+  },
+  "hash": "b5c60b008ae444cf0d5c8a33b1ad40e74c8eb313b4bf8cf623a3289eac59f55e"
+}
```
