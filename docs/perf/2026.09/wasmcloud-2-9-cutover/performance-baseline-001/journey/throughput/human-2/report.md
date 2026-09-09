## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **16** | 32 (21.85 → 45.26 ms) | ×0.86 | 1442 | 16 | 21.85 ms |
| `nodb` | **4** | 8 (1.52 → 3.92 ms) | ×1.19 | 11955 | 64 | 12.77 ms |
| `pg` | **8** | 16 (0.21 → 1.00 ms) | ×0.86 | 87068 | 8 | 0.21 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, human PAT, repetition 2`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 318 | 2.77 | 11.01 | 0 | 1 | 3183 | 926 | postgres=2 project=15 | 1.19 | 4.9 | 0 % | 0.14 |
| 4 | 607 | 5.84 | 16.44 | 0 | 4 | 6063 | 1607 | postgres=2 project=15 | 1.57 | 3.6 | 0 % | 0.29 |
| 8 | 999 | 6.66 | 26.20 | 0 | 8 | 9989 | 2769 | postgres=2 project=15 | 2.10 | 2.9 | 0 % | 0.42 |
| 16 | 1442 | 10.40 | 21.85 | 0 | 16 | 14410 | 4129 | postgres=2 project=15 | 2.81 | 2.7 | 0 % | 0.56 |
| 32 | 1237 | 25.16 | 45.26 | 0 | 32 | 12344 | 3683 | postgres=2 project=15 | 2.58 | 2.9 | 0 % | 0.48 |
| 64 | 1189 | 51.91 | 87.26 | 0 | 64 | 11835 | 3414 | postgres=2 project=15 | 2.53 | 2.9 | 0 % | 0.47 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2300 | 0.39 | 1.09 | 0 | 1 | 23004 | 354 | postgres=2 project=15 | 1.15 | 0.7 | 0 % | 0.00 |
| 4 | 6812 | 0.52 | 1.52 | 0 | 4 | 68130 | 0 | postgres=2 project=15 | 2.63 | 0.5 | 0 % | 0.00 |
| 8 | 8090 | 0.81 | 3.92 | 0 | 8 | 80914 | 1 | postgres=2 project=15 | 2.94 | 0.5 | 0 % | 0.00 |
| 16 | 10741 | 1.40 | 3.88 | 0 | 16 | 107412 | 1 | postgres=2 project=15 | 3.65 | 0.5 | 0 % | 0.00 |
| 32 | 10861 | 2.78 | 7.51 | 0 | 32 | 108616 | 0 | postgres=2 project=15 | 3.60 | 0.5 | 0 % | 0.00 |
| 64 | 11955 | 5.13 | 12.77 | 0 | 64 | 119574 | 0 | postgres=2 project=15 | 3.80 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--vg29mn11 as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 27611 | 0.03 | 0.08 | 0 | 0 | 275713 | 19919 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 0.50 |
| 4 | 54482 | 0.07 | 0.14 | 0 | 0 | 544255 | 39261 | postgres=2 project=15 | 0.03 | 0.0 | 0 % | 1.90 |
| 8 | 87068 | 0.08 | 0.21 | 0 | 0 | 869219 | 62182 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.32 |
| 16 | 75029 | 0.17 | 1.00 | 0 | 0 | 747945 | 54633 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.17 |
| 32 | 78924 | 0.31 | 1.67 | 0 | 0 | 782458 | 56496 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.36 |
| 64 | 76752 | 0.58 | 4.63 | 0 | 0 | 756767 | 54880 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.45 |
