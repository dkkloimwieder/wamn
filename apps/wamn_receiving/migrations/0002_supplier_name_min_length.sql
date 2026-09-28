ALTER TABLE receiving.supplier
    ADD CONSTRAINT supplier_name_check
    CHECK (char_length(btrim(name)) >= 1);
