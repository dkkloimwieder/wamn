SELECT status
FROM purchase_order
WHERE id = $1
FOR UPDATE;
