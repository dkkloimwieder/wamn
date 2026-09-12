PGPASSWORD=<private fixture password> PGCONNECT_TIMEOUT=5 psql -X -w -h 127.0.0.1 -p 32821 -U postgres -d postgres -Atqc SELECT\ 1
