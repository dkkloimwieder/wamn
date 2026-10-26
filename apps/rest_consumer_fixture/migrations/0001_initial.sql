CREATE TABLE rest_consumer_fixture.item (
    id uuid DEFAULT gen_random_uuid() CONSTRAINT item_id_pkey PRIMARY KEY,
    key text NOT NULL,
    name text NOT NULL,
    external_system_id uuid NOT NULL CONSTRAINT item_external_system_id_fkey REFERENCES platform.external_system (id),
    external_id text NOT NULL,
    CONSTRAINT item_external_system_id_external_id_key UNIQUE (external_system_id, external_id)
);
