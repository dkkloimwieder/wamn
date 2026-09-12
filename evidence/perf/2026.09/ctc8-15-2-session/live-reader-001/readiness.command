PGPASSWORD=<private fixture password> PGCONNECT_TIMEOUT=5 psql -X -w -h 127.0.0.1 -p 32810 -U postgres -d wamn_session_role_reader_proof -Atqc SELECT\ 1
