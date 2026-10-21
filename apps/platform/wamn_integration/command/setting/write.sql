UPDATE setting SET value = $2 WHERE key = $1 RETURNING key;
