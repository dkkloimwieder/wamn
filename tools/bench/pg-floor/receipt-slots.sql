-- Reserve the receipts of the record_receipt replay (receipt.sql) on the
-- 1000-order floor seed. psql sets :clients and :lines.
-- Client c owns the open orders n with (n - 1) % clients = c, as update.sql
-- reserves its rows. Window w of order n holds lines w * lines + 1 to
-- (w + 1) * lines. Each pass visits every window once, so 4 passes put at
-- most quantity 4 on a line, and the seed orders at least 5 of each line.
DROP TABLE IF EXISTS public.receipt_slot;
CREATE TABLE public.receipt_slot (
    client integer NOT NULL,
    slot integer NOT NULL,
    purchase_order_id uuid NOT NULL,
    line text NOT NULL,
    PRIMARY KEY (client, slot)
);
INSERT INTO public.receipt_slot (client, slot, purchase_order_id, line)
SELECT (n - 1) % :clients,
       row_number() OVER (PARTITION BY (n - 1) % :clients ORDER BY pass, n, w),
       md5('wamn-receiving-seed:order:' || n)::uuid,
       (SELECT jsonb_agg(jsonb_build_object(
                   'purchase_order_line_id', md5('wamn-receiving-seed:line:' || n || ':' || k)::uuid,
                   'quantity', '1',
                   'location_id', md5('wamn-receiving-seed:location:1')::uuid) ORDER BY k)
          FROM generate_series(w * :lines + 1, (w + 1) * :lines) AS k)::text
FROM generate_series(1, 1000) AS n,
     LATERAL generate_series(0, n / :lines - 1) AS w,
     generate_series(1, 4) AS pass
WHERE n % 5 <> 0 AND n % 7 <> 0;
GRANT SELECT ON public.receipt_slot TO wamn_app;
SELECT :lines AS lines, sum(per_client) AS slots, min(per_client) AS fewest_per_client
FROM (SELECT count(*) AS per_client FROM public.receipt_slot GROUP BY client) AS clients;
