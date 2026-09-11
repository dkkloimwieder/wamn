SELECT
    location.id,
    location.location_code
FROM location AS location
ORDER BY
    location.location_code ASC,
    location.id ASC;
