CREATE SCHEMA file_fixture;
CREATE TABLE file_fixture.document (
    id uuid NOT NULL,
    file_id uuid NOT NULL,
    CONSTRAINT document_id_pkey PRIMARY KEY (id),
    CONSTRAINT document_file_id_fkey FOREIGN KEY (file_id) REFERENCES platform.file(id)
);
