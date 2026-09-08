docker create --pull=never --name wamn-ctc8-15-2-exchange-002-pg --label wamn.proof.owner=ctc8-15-2-session-live --label wamn.proof.run=live-exchange-002 --env-file <private fixture file> -p 127.0.0.1::5432 postgres:18
docker start wamn-ctc8-15-2-exchange-002-pg
