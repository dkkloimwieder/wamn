PGPASSWORD=<private fixture password> PGCONNECT_TIMEOUT=5 psql -X -w -h 127.0.0.1 -p 32827 -U postgres -d wamn_system -Atqc SELECT\ 1
