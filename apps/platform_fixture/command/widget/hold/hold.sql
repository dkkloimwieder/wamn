SELECT (SELECT count(id) FROM widget) AS "held?" FROM pg_sleep(4);
