SELECT json_build_object(
  'claims', (SELECT count(*) FROM receiving.record_receipt_command WHERE purchase_order_id = 'ec359b6d-3db6-4e47-8b9a-711f1e451219'),
  'receipts', (SELECT count(*) FROM receiving.receipt WHERE purchase_order_id = 'ec359b6d-3db6-4e47-8b9a-711f1e451219'),
  'order', (SELECT json_build_object('status', status, 'row_version', row_version)
    FROM receiving.purchase_order WHERE id = 'ec359b6d-3db6-4e47-8b9a-711f1e451219'),
  'lines', (SELECT json_agg(json_build_object('id', id, 'received', received_quantity::text) ORDER BY line_number)
    FROM receiving.purchase_order_line WHERE purchase_order_id = 'ec359b6d-3db6-4e47-8b9a-711f1e451219'),
  'receipt_lines', (SELECT COALESCE(json_agg(json_build_object(
    'line', line.purchase_order_line_id, 'quantity', line.quantity::text,
    'location', line.location_id)), '[]'::json)
    FROM receiving.receipt_line AS line JOIN receiving.receipt AS receipt ON receipt.id = line.receipt_id
    WHERE receipt.purchase_order_id = 'ec359b6d-3db6-4e47-8b9a-711f1e451219'),
  'canonical', (SELECT COALESCE(json_agg(convert_from(canonical_command, 'UTF8')::json), '[]'::json)
    FROM receiving.record_receipt_command WHERE purchase_order_id = 'ec359b6d-3db6-4e47-8b9a-711f1e451219'));
