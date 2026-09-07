CREATE TABLE receiving.carrier (
    id uuid CONSTRAINT carrier_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT carrier_name_check CHECK (length(name) > 0)
);

CREATE TABLE receiving.dock (
    id uuid CONSTRAINT dock_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT dock_name_check CHECK (length(name) > 0)
);

-- One appointment joins one carrier to one dock for one slot. Two appointments
-- on one dock never overlap: every booking takes the dock row FOR UPDATE
-- first, so the overlap probe and the insert that follows it are serialized
-- per dock and no second booking can slip between them.
CREATE TABLE receiving.appointment (
    id uuid CONSTRAINT appointment_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    carrier_id uuid NOT NULL
        CONSTRAINT appointment_carrier_id_fkey
        REFERENCES receiving.carrier (id),
    dock_id uuid NOT NULL
        CONSTRAINT appointment_dock_id_fkey
        REFERENCES receiving.dock (id),
    slot_start timestamptz NOT NULL,
    slot_end timestamptz NOT NULL,
    status text NOT NULL DEFAULT 'scheduled',
    arrived_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT appointment_slot_start_slot_end_check CHECK (slot_end > slot_start),
    CONSTRAINT appointment_status_check
        CHECK (status IN ('scheduled', 'arrived', 'departed')),
    CONSTRAINT appointment_status_arrived_at_check
        CHECK (
            (status = 'scheduled' AND arrived_at IS NULL)
            OR (status <> 'scheduled' AND arrived_at IS NOT NULL)
        )
);

CREATE TABLE receiving.carrier_create_command (
    idempotency_key text
        CONSTRAINT carrier_create_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL,
    carrier_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT carrier_create_command_carrier_id_key UNIQUE,
    finalized boolean,
    CONSTRAINT carrier_create_command_canonical_command_check
        CHECK (octet_length(canonical_command) > 0),
    CONSTRAINT carrier_create_command_finalized_check
        CHECK (finalized IS NULL OR finalized)
);

CREATE TABLE receiving.dock_create_command (
    idempotency_key text
        CONSTRAINT dock_create_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL,
    dock_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT dock_create_command_dock_id_key UNIQUE,
    finalized boolean,
    CONSTRAINT dock_create_command_canonical_command_check
        CHECK (octet_length(canonical_command) > 0),
    CONSTRAINT dock_create_command_finalized_check
        CHECK (finalized IS NULL OR finalized)
);

CREATE TABLE receiving.appointment_book_command (
    idempotency_key text
        CONSTRAINT appointment_book_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL,
    appointment_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT appointment_book_command_appointment_id_key UNIQUE,
    status text,
    CONSTRAINT appointment_book_command_canonical_command_check
        CHECK (octet_length(canonical_command) > 0),
    CONSTRAINT appointment_book_command_status_check
        CHECK (status IS NULL OR status = 'scheduled')
);

CREATE TABLE receiving.appointment_check_in_command (
    idempotency_key text
        CONSTRAINT appointment_check_in_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL,
    check_in_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT appointment_check_in_command_check_in_id_key UNIQUE,
    status text,
    arrived_at timestamptz,
    CONSTRAINT appointment_check_in_command_canonical_command_check
        CHECK (octet_length(canonical_command) > 0),
    CONSTRAINT appointment_check_in_command_status_check
        CHECK (status IS NULL OR status = 'arrived'),
    CONSTRAINT appointment_check_in_command_status_arrived_at_check
        CHECK (
            (status IS NULL AND arrived_at IS NULL)
            OR (status IS NOT NULL AND arrived_at IS NOT NULL)
        )
);
