ALTER TABLE wms.product
    ADD CONSTRAINT product_product_code_check
    CHECK (char_length(btrim(product_code)) >= 1);

ALTER TABLE wms.location
    ADD CONSTRAINT location_location_code_check
    CHECK (char_length(btrim(location_code)) >= 1);

ALTER TABLE wms.packaging
    ADD CONSTRAINT packaging_packaging_code_check
    CHECK (char_length(btrim(packaging_code)) >= 1);
