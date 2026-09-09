## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (17.77 → 24.43 ms) | ×1.09 | 1417 | 16 | 24.43 ms |
| `nodb` | **8** | 16 (1.99 → 3.14 ms) | ×1.05 | 11962 | 16 | 3.14 ms |
| `pg` | **8** | 16 (0.25 → 0.58 ms) | ×1.08 | 86440 | 16 | 0.58 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, service PAT, repetition 1`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 357 | 2.62 | 6.51 | 0 | 1 | 3567 | 738 | postgres=2 project=4 | 0.94 | 4.6 | 0 % | 0.10 |
| 4 | 1046 | 3.50 | 8.12 | 0 | 4 | 10462 | 2842 | postgres=2 project=8 | 2.31 | 3.0 | 0 % | 0.37 |
| 8 | 1301 | 5.41 | 17.77 | 0 | 8 | 13006 | 3764 | postgres=2 project=12 | 2.55 | 2.7 | 0 % | 0.45 |
| 16 | 1417 | 10.65 | 24.43 | 0 | 16 | 14158 | 4175 | postgres=2 project=13 | 2.83 | 2.7 | 0 % | 0.50 |
| 32 | 1084 | 28.21 | 57.16 | 0 | 32 | 10812 | 3187 | postgres=2 project=15 | 2.33 | 3.0 | 0 % | 0.40 |
| 64 | 1044 | 59.80 | 100.80 | 0 | 64 | 10383 | 3004 | postgres=2 project=15 | 2.33 | 3.1 | 0 % | 0.39 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2586 | 0.36 | 0.82 | 0 | 1 | 25864 | 321 | postgres=2 project=15 | 1.18 | 0.6 | 0 % | 0.00 |
| 4 | 6471 | 0.53 | 1.77 | 0 | 4 | 64717 | 1 | postgres=2 project=15 | 2.52 | 0.5 | 0 % | 0.00 |
| 8 | 11383 | 0.63 | 1.99 | 0 | 8 | 113841 | 1 | postgres=2 project=15 | 3.73 | 0.5 | 0 % | 0.00 |
| 16 | 11962 | 1.27 | 3.14 | 0 | 16 | 119654 | 0 | postgres=2 project=15 | 3.85 | 0.4 | 0 % | 0.00 |
| 32 | 11065 | 2.73 | 7.34 | 0 | 32 | 110655 | 0 | postgres=2 project=15 | 3.65 | 0.5 | 0 % | 0.00 |
| 64 | 9987 | 5.99 | 17.02 | 0 | 63 | 99850 | 1 | postgres=2 project=15 | 3.44 | 0.5 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--vg29mn11 as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 25560 | 0.03 | 0.09 | 0 | 0 | 255454 | 12230 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 0.33 |
| 4 | 56196 | 0.07 | 0.14 | 0 | 0 | 561234 | 40095 | postgres=2 project=15 | 0.03 | 0.0 | 0 % | 1.90 |
| 8 | 79879 | 0.08 | 0.25 | 0 | 0 | 797349 | 58484 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.29 |
| 16 | 86440 | 0.17 | 0.58 | 0 | 0 | 861764 | 62450 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.48 |
| 32 | 82042 | 0.32 | 1.42 | 0 | 0 | 815544 | 59205 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.43 |
| 64 | 78183 | 0.54 | 4.42 | 0 | 0 | 772668 | 55903 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.40 |
