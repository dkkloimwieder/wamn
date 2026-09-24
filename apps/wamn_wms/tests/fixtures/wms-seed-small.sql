-- The small WMS dataset, saved from `wms-seed.sql -v scale=10`.
--
-- 10 products, 10 locations, 10 pallets (one held), and 19 quantity rows.
-- Apply it to an empty WMS schema instead of building the rows again:
--
--   psql "$TARGET_DATABASE_URL" -f wms-seed-small.sql
--
-- The medium and large sizes are not saved here. Build them with the generator
-- and keep them outside the repository.

--
-- PostgreSQL database dump
--

\restrict QXrj9WJ4F2lMNeyqBWQichHKe0folja7VMMLr6vffIueEUxGeJwtrhzlkh61qcA

-- Dumped from database version 18.6 (Ubuntu 18.6-1.pgdg26.04+2)
-- Dumped by pg_dump version 18.6 (Ubuntu 18.6-1.pgdg26.04+2)

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET transaction_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

-- The record history trigger requires an actor, and a dump carries none.
-- These settings hold for the whole session that applies this file.
SELECT set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', false),
       set_config('app.tenant_id', 'wms-route-auth', false),
       set_config('app.operation', 'admin:seed-wms-fixture', false);

--
-- Data for Name: location; Type: TABLE DATA; Schema: wms; Owner: postgres
--

COPY wms.location (id, location_code, row_version, created_at) FROM stdin;
97d88290-feb6-1874-f784-7ae7603e645c	LOC-0001	1	2026-09-23 23:12:54.096232-04
f1404530-787c-bde1-4f30-638b22f1f5eb	LOC-0002	1	2026-09-23 23:12:54.096232-04
63644449-8564-63c6-e393-bbe7d08a9585	LOC-0003	1	2026-09-23 23:12:54.096232-04
095b3366-1f21-8f78-0edd-e2fe185e1393	LOC-0004	1	2026-09-23 23:12:54.096232-04
422615b3-2406-0aa7-4254-65f2f0240528	LOC-0005	1	2026-09-23 23:12:54.096232-04
79bbac34-37fd-fa09-cf41-325a418dd493	LOC-0006	1	2026-09-23 23:12:54.096232-04
6b394bae-b617-0ca3-6eba-d782fb0891f2	LOC-0007	1	2026-09-23 23:12:54.096232-04
f2d6c4d1-b634-cab2-45a9-f9557bd3d0f5	LOC-0008	1	2026-09-23 23:12:54.096232-04
2778a558-9ad2-dd7d-9883-20d87cf6cd7a	LOC-0009	1	2026-09-23 23:12:54.096232-04
a329d423-7dfa-4074-011a-5927fd7d3b52	LOC-0010	1	2026-09-23 23:12:54.096232-04
\.


--
-- Data for Name: pallet; Type: TABLE DATA; Schema: wms; Owner: postgres
--

COPY wms.pallet (id, pallet_code, location_id, status, row_version, created_at, created_by, updated_at, updated_by) FROM stdin;
82b2ffa1-142a-20e7-bc58-93751f88bd7d	PAL-000001	97d88290-feb6-1874-f784-7ae7603e645c	available	1	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b
7d89dfbe-cae7-8155-e8e8-30f9dfba39bb	PAL-000002	f1404530-787c-bde1-4f30-638b22f1f5eb	available	1	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b
f96a90b3-5e7d-b56d-8348-fd8b0dda30da	PAL-000003	63644449-8564-63c6-e393-bbe7d08a9585	available	1	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b
14b7fca1-1e92-1311-76a2-6f3c2e8b2c84	PAL-000004	095b3366-1f21-8f78-0edd-e2fe185e1393	available	1	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b
8d22212a-94e6-281c-5dd7-2f76d8846eca	PAL-000005	422615b3-2406-0aa7-4254-65f2f0240528	available	1	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b
252da141-b014-fe2f-ebe0-103105cd75cc	PAL-000006	79bbac34-37fd-fa09-cf41-325a418dd493	available	1	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b
e491cc0c-f7c1-ae32-ce04-9b94f8d26187	PAL-000007	6b394bae-b617-0ca3-6eba-d782fb0891f2	available	1	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b
286ee4ec-5761-dc3d-9561-507f9c3d1a2e	PAL-000008	f2d6c4d1-b634-cab2-45a9-f9557bd3d0f5	available	1	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b
83357d7d-d13d-7045-29a8-f54fd1e4480e	PAL-000009	2778a558-9ad2-dd7d-9883-20d87cf6cd7a	available	1	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b
aeed8931-c07b-3943-89f3-b76b892762fe	PAL-000010	a329d423-7dfa-4074-011a-5927fd7d3b52	held	1	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-23 23:12:54.096232-04	770df186-ac15-579e-b46b-c297cae2011b
\.


--
-- Data for Name: product; Type: TABLE DATA; Schema: wms; Owner: postgres
--

COPY wms.product (id, product_code, row_version, created_at) FROM stdin;
d1c1a7de-1ec6-acc6-9763-11fbd91c01a4	SKU-00001	1	2026-09-23 23:12:54.096232-04
5bece3de-7932-850a-9aca-97e02054075d	SKU-00002	1	2026-09-23 23:12:54.096232-04
91d0f2a8-1a86-a4e4-5224-31b4cd587772	SKU-00003	1	2026-09-23 23:12:54.096232-04
aa6ffc3e-df0a-b787-f9fc-0159eaa57881	SKU-00004	1	2026-09-23 23:12:54.096232-04
74338768-d9df-21e5-3450-6732ba6f649c	SKU-00005	1	2026-09-23 23:12:54.096232-04
e47f8264-25cb-b63b-9272-4481c56fb5c3	SKU-00006	1	2026-09-23 23:12:54.096232-04
bf590122-9881-4570-8186-61bee49d251f	SKU-00007	1	2026-09-23 23:12:54.096232-04
84b5e498-cbf5-b0a9-3da9-8102f6c36a0e	SKU-00008	1	2026-09-23 23:12:54.096232-04
5b895760-101d-42d6-e980-fa75400f198e	SKU-00009	1	2026-09-23 23:12:54.096232-04
76eb6329-ea95-d3b1-8423-6922913a3690	SKU-00010	1	2026-09-23 23:12:54.096232-04
\.


--
-- Data for Name: pallet_quantity; Type: TABLE DATA; Schema: wms; Owner: postgres
--

COPY wms.pallet_quantity (id, pallet_id, product_id, status, quantity, created_at) FROM stdin;
25d8a255-0f4e-ce84-0aa9-4f27953302ce	82b2ffa1-142a-20e7-bc58-93751f88bd7d	d1c1a7de-1ec6-acc6-9763-11fbd91c01a4	available	30	2026-09-23 23:12:54.096232-04
9a0ca1e7-ceb1-b780-be05-49569b416d95	7d89dfbe-cae7-8155-e8e8-30f9dfba39bb	d1c1a7de-1ec6-acc6-9763-11fbd91c01a4	available	37	2026-09-23 23:12:54.096232-04
ccb7134e-5b91-dc5b-6bb5-3202c4cee9ee	7d89dfbe-cae7-8155-e8e8-30f9dfba39bb	5bece3de-7932-850a-9aca-97e02054075d	available	50	2026-09-23 23:12:54.096232-04
69465d93-e691-c293-01b3-12c1491024bd	f96a90b3-5e7d-b56d-8348-fd8b0dda30da	5bece3de-7932-850a-9aca-97e02054075d	available	44	2026-09-23 23:12:54.096232-04
ef776c4b-7f4a-bd8d-da06-7b4279427e56	f96a90b3-5e7d-b56d-8348-fd8b0dda30da	91d0f2a8-1a86-a4e4-5224-31b4cd587772	available	57	2026-09-23 23:12:54.096232-04
d28b0308-7f0f-5383-8bed-5d45c47c97f8	f96a90b3-5e7d-b56d-8348-fd8b0dda30da	aa6ffc3e-df0a-b787-f9fc-0159eaa57881	available	70	2026-09-23 23:12:54.096232-04
9c320656-b1f9-f3a2-c091-1df8c804e579	14b7fca1-1e92-1311-76a2-6f3c2e8b2c84	5bece3de-7932-850a-9aca-97e02054075d	available	51	2026-09-23 23:12:54.096232-04
c93cfabe-60c5-da6c-b45c-c51040b6ced0	8d22212a-94e6-281c-5dd7-2f76d8846eca	91d0f2a8-1a86-a4e4-5224-31b4cd587772	available	58	2026-09-23 23:12:54.096232-04
53fcd4ac-e2dd-37fa-074c-e4e92084084b	8d22212a-94e6-281c-5dd7-2f76d8846eca	aa6ffc3e-df0a-b787-f9fc-0159eaa57881	available	71	2026-09-23 23:12:54.096232-04
52a8e369-66dc-2698-6694-efd200557003	252da141-b014-fe2f-ebe0-103105cd75cc	91d0f2a8-1a86-a4e4-5224-31b4cd587772	available	65	2026-09-23 23:12:54.096232-04
8a97ed68-7036-1e3c-6ec5-d7f2eb2f7b44	252da141-b014-fe2f-ebe0-103105cd75cc	aa6ffc3e-df0a-b787-f9fc-0159eaa57881	available	78	2026-09-23 23:12:54.096232-04
343bf384-ed5d-9d11-4780-36b801128e14	252da141-b014-fe2f-ebe0-103105cd75cc	74338768-d9df-21e5-3450-6732ba6f649c	available	91	2026-09-23 23:12:54.096232-04
9196e6e4-30e1-fd16-eb26-46398c8b38cd	e491cc0c-f7c1-ae32-ce04-9b94f8d26187	aa6ffc3e-df0a-b787-f9fc-0159eaa57881	available	72	2026-09-23 23:12:54.096232-04
a0a135fe-479e-f7ff-e842-39a117c3bfda	286ee4ec-5761-dc3d-9561-507f9c3d1a2e	aa6ffc3e-df0a-b787-f9fc-0159eaa57881	available	79	2026-09-23 23:12:54.096232-04
b092f3c8-d062-abbe-c406-1ded4dff3a32	286ee4ec-5761-dc3d-9561-507f9c3d1a2e	74338768-d9df-21e5-3450-6732ba6f649c	available	92	2026-09-23 23:12:54.096232-04
cef54983-f8eb-f954-48f4-304da42a0c07	83357d7d-d13d-7045-29a8-f54fd1e4480e	74338768-d9df-21e5-3450-6732ba6f649c	available	86	2026-09-23 23:12:54.096232-04
31e0a228-43d6-065e-4380-83b41a2e4533	83357d7d-d13d-7045-29a8-f54fd1e4480e	e47f8264-25cb-b63b-9272-4481c56fb5c3	available	99	2026-09-23 23:12:54.096232-04
9df6e734-3e62-f1f4-ac91-6f75efef0896	83357d7d-d13d-7045-29a8-f54fd1e4480e	bf590122-9881-4570-8186-61bee49d251f	available	21	2026-09-23 23:12:54.096232-04
c7746f0d-3f58-be42-931c-b2a76a1d6646	aeed8931-c07b-3943-89f3-b76b892762fe	74338768-d9df-21e5-3450-6732ba6f649c	held	93	2026-09-23 23:12:54.096232-04
\.


\unrestrict QXrj9WJ4F2lMNeyqBWQichHKe0folja7VMMLr6vffIueEUxGeJwtrhzlkh61qcA

