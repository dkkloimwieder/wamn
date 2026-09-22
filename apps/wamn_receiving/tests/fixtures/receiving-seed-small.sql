-- The small Receiving dataset, saved from `receiving-seed.sql -v scale=10`.
--
-- 10 items, 10 locations, 10 purchase orders, and 55 lines. Apply it to an
-- empty Receiving schema instead of building the rows again:
--
--   psql "$TARGET_DATABASE_URL" -f receiving-seed-small.sql
--
-- The medium and large sizes are not saved here. Build them with the generator
-- and keep them outside the repository.

--
-- PostgreSQL database dump
--

\restrict ya8Eqr81PyN9AVLmDIhvsPXPQRzpfFg9cPqUjunB4SIidWoDOaHEH1yxeu3lOE7

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
-- These two settings hold for the whole session that applies this file.
SELECT set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', false),
       set_config('app.tenant_id', 'receiving-route-auth', false),
       set_config('app.operation', 'admin:seed-receiving-fixture', false);

--
-- Data for Name: item; Type: TABLE DATA; Schema: receiving; Owner: wamn_db_owner
--

COPY receiving.item (id, item_number) FROM stdin;
660ce2a7-686f-0fb8-1e6d-279ae458a8d4	ITEM-0001
da15160b-7de4-959d-c032-c991a52cfce2	ITEM-0002
5a08babd-5666-a573-b988-cd6ffbf79496	ITEM-0003
5df757ae-eb63-8a2a-95f0-68f187b9837f	ITEM-0004
38ab7a49-c3a7-a016-1cd7-d23f6be6bcd9	ITEM-0005
d1dbc350-b239-5a87-6399-4b1588b678ec	ITEM-0006
aeb376b4-6fee-e3bf-58dd-4a3726e2228e	ITEM-0007
e29ca927-d6ad-3031-e198-447f41b2c629	ITEM-0008
94b796c6-2520-eade-00c1-b135c819b8d5	ITEM-0009
2cf71d2c-06ee-487b-cab2-b56605ba6d3d	ITEM-0010
\.


--
-- Data for Name: location; Type: TABLE DATA; Schema: receiving; Owner: wamn_db_owner
--

COPY receiving.location (id, location_code) FROM stdin;
cd8a047e-4f75-111b-7165-0e00cfcd529b	DOCK-0001
17a9db73-b31f-d079-905f-6e135e2ebce8	DOCK-0002
79f6f9ed-46b6-aea0-4eb5-b2631f956824	DOCK-0003
f949d766-b120-25d9-4586-c5b9b9d8acf8	DOCK-0004
ccfd1a6b-c92c-de1a-4c10-dc713ad67adb	DOCK-0005
4f135c51-cd72-7e6a-6216-26f12430ea86	DOCK-0006
bb581d2f-40d2-10c5-618b-4bd53b6657d1	DOCK-0007
4f258111-9862-ea15-8531-c2fde01ff3e8	DOCK-0008
9d2e6c6e-beca-5078-1985-ed73367d5e27	DOCK-0009
bab357b1-0164-9377-a1c2-38475a5e134d	DOCK-0010
\.


--
-- Data for Name: purchase_order; Type: TABLE DATA; Schema: receiving; Owner: wamn_db_owner
--

COPY receiving.purchase_order (id, purchase_order_number, supplier_id, status, row_version, created_at, created_by, updated_at, updated_by) FROM stdin;
a9d20c8e-13eb-f69d-e6e8-96faa5930d97	PO-0001	db63a16b-c72d-08e0-5944-2bf833f4800a	open	1	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b
6ea8f898-6367-c762-8de5-528f23fa1f08	PO-0002	ba625e07-5740-3091-db51-6944a7519f05	open	1	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b
37eabe22-7bc1-da4c-cc39-ed92d921562c	PO-0003	f603e48c-3f5a-61ba-8aa9-a72818dc18ca	open	1	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b
0d979896-06a5-872e-97b5-af20a9af8a7d	PO-0004	8ca2033b-6260-1fe8-a061-e0c0ce1e3c2f	open	1	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b
7f42d0ff-33d7-d861-1813-f43fdca1b25f	PO-0005	8c62c320-75c8-f874-c7c7-206d04daac57	complete	1	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b
0530d327-a4da-ac37-0be0-87d800aa9370	PO-0006	db63a16b-c72d-08e0-5944-2bf833f4800a	open	1	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b
640de8a4-f56f-840d-8e6c-18dc55f2c766	PO-0007	ba625e07-5740-3091-db51-6944a7519f05	cancelled	1	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b
adda4f87-15af-82ce-1852-1b2409879e95	PO-0008	f603e48c-3f5a-61ba-8aa9-a72818dc18ca	open	1	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b
5b6eb305-806c-c8ba-8b8b-c658ced1f78a	PO-0009	8ca2033b-6260-1fe8-a061-e0c0ce1e3c2f	open	1	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b
28949ddd-af64-b13f-7b52-8b5db8ef4756	PO-0010	8c62c320-75c8-f874-c7c7-206d04daac57	complete	1	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b	2026-09-21 23:05:54.33672-04	770df186-ac15-579e-b46b-c297cae2011b
\.


--
-- Data for Name: purchase_order_line; Type: TABLE DATA; Schema: receiving; Owner: wamn_db_owner
--

COPY receiving.purchase_order_line (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) FROM stdin;
ebdc6fa8-57c3-1d5e-5d68-e5a01d066f9b	a9d20c8e-13eb-f69d-e6e8-96faa5930d97	1	94b796c6-2520-eade-00c1-b135c819b8d5	7	0
e76a35e6-f7cd-3147-938c-954600262e3d	6ea8f898-6367-c762-8de5-528f23fa1f08	1	d1dbc350-b239-5a87-6399-4b1588b678ec	8	0
2abeb99d-1ff6-a208-90ca-ba5522145dbb	6ea8f898-6367-c762-8de5-528f23fa1f08	2	aeb376b4-6fee-e3bf-58dd-4a3726e2228e	9	0
5c55c6ba-8acf-81bd-cf2e-e768231454ec	37eabe22-7bc1-da4c-cc39-ed92d921562c	1	5a08babd-5666-a573-b988-cd6ffbf79496	9	0
648fa78e-4b54-0436-88cc-d3d4b756141f	37eabe22-7bc1-da4c-cc39-ed92d921562c	2	5df757ae-eb63-8a2a-95f0-68f187b9837f	10	0
2b9a2302-c8c9-9394-32f4-0b48d0c26338	37eabe22-7bc1-da4c-cc39-ed92d921562c	3	38ab7a49-c3a7-a016-1cd7-d23f6be6bcd9	11	0
21f0098e-51a3-8124-b013-abfff471cdd1	0d979896-06a5-872e-97b5-af20a9af8a7d	1	2cf71d2c-06ee-487b-cab2-b56605ba6d3d	10	0
132ac5d3-84c6-e88c-f5c2-c4e6ba75cd2c	0d979896-06a5-872e-97b5-af20a9af8a7d	2	660ce2a7-686f-0fb8-1e6d-279ae458a8d4	11	0
f95e0a7e-cc36-d5bd-0d99-bd8c9b6b0efd	0d979896-06a5-872e-97b5-af20a9af8a7d	3	da15160b-7de4-959d-c032-c991a52cfce2	12	0
7129b1ac-feab-b702-696e-0bac550b6a03	0d979896-06a5-872e-97b5-af20a9af8a7d	4	5a08babd-5666-a573-b988-cd6ffbf79496	13	0
07f7ed81-2805-8d07-0d57-7bc5c2fb3f91	7f42d0ff-33d7-d861-1813-f43fdca1b25f	1	aeb376b4-6fee-e3bf-58dd-4a3726e2228e	11	0
475acf27-1875-eb99-5090-3ce7da4506d1	7f42d0ff-33d7-d861-1813-f43fdca1b25f	2	e29ca927-d6ad-3031-e198-447f41b2c629	12	0
ec065f2d-8ab1-830a-f50f-27fb36f43642	7f42d0ff-33d7-d861-1813-f43fdca1b25f	3	94b796c6-2520-eade-00c1-b135c819b8d5	13	0
1b401c5f-c22a-92d4-f04f-c07e6335beee	7f42d0ff-33d7-d861-1813-f43fdca1b25f	4	2cf71d2c-06ee-487b-cab2-b56605ba6d3d	14	0
02d3a98e-54b8-081f-6d5b-ab50a1ff95be	7f42d0ff-33d7-d861-1813-f43fdca1b25f	5	660ce2a7-686f-0fb8-1e6d-279ae458a8d4	15	0
f2d72234-e4f4-1255-eeb2-c80b9f12d51c	0530d327-a4da-ac37-0be0-87d800aa9370	1	5df757ae-eb63-8a2a-95f0-68f187b9837f	12	0
576ab0de-747c-d89e-157b-7c9e53ca6da9	0530d327-a4da-ac37-0be0-87d800aa9370	2	38ab7a49-c3a7-a016-1cd7-d23f6be6bcd9	13	0
27682b5a-4298-738b-197e-98fdce4b147e	0530d327-a4da-ac37-0be0-87d800aa9370	3	d1dbc350-b239-5a87-6399-4b1588b678ec	14	0
03b74a41-6dfa-250b-5449-bd30684afb70	0530d327-a4da-ac37-0be0-87d800aa9370	4	aeb376b4-6fee-e3bf-58dd-4a3726e2228e	15	0
2d556e91-fb9d-af7e-837b-258caffab522	0530d327-a4da-ac37-0be0-87d800aa9370	5	e29ca927-d6ad-3031-e198-447f41b2c629	16	0
9ae9eebe-ae9a-0ecb-7192-a984059c7f4b	0530d327-a4da-ac37-0be0-87d800aa9370	6	94b796c6-2520-eade-00c1-b135c819b8d5	17	0
b3a0c7ad-886c-9690-c791-29f32d72f81b	640de8a4-f56f-840d-8e6c-18dc55f2c766	1	660ce2a7-686f-0fb8-1e6d-279ae458a8d4	13	0
34597301-a86c-c1fe-cccd-55cf31186833	640de8a4-f56f-840d-8e6c-18dc55f2c766	2	da15160b-7de4-959d-c032-c991a52cfce2	14	0
11458d22-b2a1-35c7-166b-9bed1f7ee6cd	640de8a4-f56f-840d-8e6c-18dc55f2c766	3	5a08babd-5666-a573-b988-cd6ffbf79496	15	0
e6faa42a-d9ef-d46d-d59c-e07bcfa745c4	640de8a4-f56f-840d-8e6c-18dc55f2c766	4	5df757ae-eb63-8a2a-95f0-68f187b9837f	16	0
d823ff50-c879-cc5d-0fae-f09e15924911	640de8a4-f56f-840d-8e6c-18dc55f2c766	5	38ab7a49-c3a7-a016-1cd7-d23f6be6bcd9	17	0
b92d5fe7-2c37-a402-89e8-d1a52c0f6f06	640de8a4-f56f-840d-8e6c-18dc55f2c766	6	d1dbc350-b239-5a87-6399-4b1588b678ec	18	0
429a96a4-46d0-9e7f-1558-5dfbbade3d5e	640de8a4-f56f-840d-8e6c-18dc55f2c766	7	aeb376b4-6fee-e3bf-58dd-4a3726e2228e	19	0
50c79a0a-c2b6-6046-c50b-d4dea4f80074	adda4f87-15af-82ce-1852-1b2409879e95	1	e29ca927-d6ad-3031-e198-447f41b2c629	14	0
bb860973-97b6-e560-7f80-ae7595fcab16	adda4f87-15af-82ce-1852-1b2409879e95	2	94b796c6-2520-eade-00c1-b135c819b8d5	15	0
d4cdbd6c-3de4-07b0-9c51-ffc6a73ee670	adda4f87-15af-82ce-1852-1b2409879e95	3	2cf71d2c-06ee-487b-cab2-b56605ba6d3d	16	0
91f7c549-3b15-61af-210c-69d1719a2b4b	adda4f87-15af-82ce-1852-1b2409879e95	4	660ce2a7-686f-0fb8-1e6d-279ae458a8d4	17	0
b28b5aff-7072-a839-2ff1-fe2cf7e950bc	adda4f87-15af-82ce-1852-1b2409879e95	5	da15160b-7de4-959d-c032-c991a52cfce2	18	0
5bcdfd92-fa66-3228-7365-6e9613eac1f4	adda4f87-15af-82ce-1852-1b2409879e95	6	5a08babd-5666-a573-b988-cd6ffbf79496	19	0
236ef2e5-9803-0b19-6b4e-63677ff2893f	adda4f87-15af-82ce-1852-1b2409879e95	7	5df757ae-eb63-8a2a-95f0-68f187b9837f	20	0
852bac23-d749-84eb-cad0-6f4cd4f4edf8	adda4f87-15af-82ce-1852-1b2409879e95	8	38ab7a49-c3a7-a016-1cd7-d23f6be6bcd9	21	0
9b8c346e-56ac-6948-de4b-f2f99f0620bc	5b6eb305-806c-c8ba-8b8b-c658ced1f78a	1	38ab7a49-c3a7-a016-1cd7-d23f6be6bcd9	15	0
f897e640-4e33-2d6a-a59a-e392863c165e	5b6eb305-806c-c8ba-8b8b-c658ced1f78a	2	d1dbc350-b239-5a87-6399-4b1588b678ec	16	0
d7005e04-4046-1c9c-3d55-2019ae7b00fc	5b6eb305-806c-c8ba-8b8b-c658ced1f78a	3	aeb376b4-6fee-e3bf-58dd-4a3726e2228e	17	0
d4f05c61-beca-0c27-f981-791737bd4b1b	5b6eb305-806c-c8ba-8b8b-c658ced1f78a	4	e29ca927-d6ad-3031-e198-447f41b2c629	18	0
1e78605c-7379-ba5d-fa5d-b7693857932a	5b6eb305-806c-c8ba-8b8b-c658ced1f78a	5	94b796c6-2520-eade-00c1-b135c819b8d5	19	0
8f91607c-526d-2c5d-ca0a-060cb95e0a0f	5b6eb305-806c-c8ba-8b8b-c658ced1f78a	6	2cf71d2c-06ee-487b-cab2-b56605ba6d3d	20	0
ce856f48-9d90-44a6-ea3b-27f051d0a73f	5b6eb305-806c-c8ba-8b8b-c658ced1f78a	7	660ce2a7-686f-0fb8-1e6d-279ae458a8d4	21	0
f540a0cd-1eb8-26c2-75bc-40f19dd91d5e	5b6eb305-806c-c8ba-8b8b-c658ced1f78a	8	da15160b-7de4-959d-c032-c991a52cfce2	22	0
83734717-cc79-6988-cb8d-ef5b369e0867	5b6eb305-806c-c8ba-8b8b-c658ced1f78a	9	5a08babd-5666-a573-b988-cd6ffbf79496	23	0
a03931bd-68a6-4414-074d-878e6f096c25	28949ddd-af64-b13f-7b52-8b5db8ef4756	1	da15160b-7de4-959d-c032-c991a52cfce2	16	0
12ea3db7-b15a-c485-b892-d26b7b28a565	28949ddd-af64-b13f-7b52-8b5db8ef4756	2	5a08babd-5666-a573-b988-cd6ffbf79496	17	0
f1b1e188-9e35-d92d-511a-b95023924031	28949ddd-af64-b13f-7b52-8b5db8ef4756	3	5df757ae-eb63-8a2a-95f0-68f187b9837f	18	0
323d079a-1a7e-fd1d-d672-b2f3e4920d76	28949ddd-af64-b13f-7b52-8b5db8ef4756	4	38ab7a49-c3a7-a016-1cd7-d23f6be6bcd9	19	0
d275e669-73d0-cb89-fdd0-ab7bb149d0d4	28949ddd-af64-b13f-7b52-8b5db8ef4756	5	d1dbc350-b239-5a87-6399-4b1588b678ec	20	0
526517e3-a201-a9d5-cb8c-5811755a7bb2	28949ddd-af64-b13f-7b52-8b5db8ef4756	6	aeb376b4-6fee-e3bf-58dd-4a3726e2228e	21	0
9891cf3b-0dba-ee1a-5fae-4d51c8b8af1c	28949ddd-af64-b13f-7b52-8b5db8ef4756	7	e29ca927-d6ad-3031-e198-447f41b2c629	22	0
2dc16345-6ae0-e472-9f52-45aa920e32b5	28949ddd-af64-b13f-7b52-8b5db8ef4756	8	94b796c6-2520-eade-00c1-b135c819b8d5	23	0
bc5be682-7cbb-8128-a894-7c0da294c1bc	28949ddd-af64-b13f-7b52-8b5db8ef4756	9	2cf71d2c-06ee-487b-cab2-b56605ba6d3d	24	0
bf7775ce-62cf-aa5e-322a-c6f9c5734135	28949ddd-af64-b13f-7b52-8b5db8ef4756	10	660ce2a7-686f-0fb8-1e6d-279ae458a8d4	5	0
\.


--
-- PostgreSQL database dump complete
--

\unrestrict ya8Eqr81PyN9AVLmDIhvsPXPQRzpfFg9cPqUjunB4SIidWoDOaHEH1yxeu3lOE7

