## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (12.75 → 28.44 ms) | ×0.96 | 1258 | 8 | 12.75 ms |
| `nodb` | **8** | 16 (2.84 → 4.34 ms) | ×1.00 | 11888 | 64 | 12.03 ms |
| `pg` | **8** | 16 (0.27 → 0.82 ms) | ×0.99 | 78746 | 8 | 0.27 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, human PAT, repetition 3`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 323 | 2.94 | 5.68 | 0 | 1 | 3229 | 1035 | postgres=2 project=18 | 1.18 | 5.1 | 0 % | 0.14 |
| 4 | 941 | 3.91 | 8.79 | 0 | 4 | 9411 | 3174 | postgres=2 project=18 | 2.14 | 3.1 | 0 % | 0.39 |
| 8 | 1258 | 5.93 | 12.75 | 0 | 8 | 12574 | 4412 | postgres=2 project=18 | 2.59 | 2.9 | 0 % | 0.51 |
| 16 | 1207 | 12.21 | 28.44 | 0 | 16 | 12060 | 4496 | postgres=2 project=18 | 2.53 | 2.9 | 0 % | 0.51 |
| 32 | 1042 | 29.38 | 54.01 | 0 | 32 | 10394 | 3589 | postgres=2 project=18 | 2.23 | 3.1 | 0 % | 0.43 |
| 64 | 1064 | 56.96 | 112.21 | 0 | 64 | 10582 | 3843 | postgres=2 project=18 | 2.34 | 3.1 | 0 % | 0.44 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2530 | 0.37 | 0.77 | 0 | 1 | 25299 | 405 | postgres=2 project=18 | 1.18 | 0.6 | 0 % | 0.00 |
| 4 | 6006 | 0.55 | 2.30 | 0 | 4 | 60085 | 1 | postgres=2 project=18 | 2.34 | 0.5 | 0 % | 0.00 |
| 8 | 9797 | 0.71 | 2.84 | 0 | 8 | 97991 | 0 | postgres=2 project=18 | 3.47 | 0.5 | 0 % | 0.00 |
| 16 | 9770 | 1.53 | 4.34 | 0 | 16 | 97714 | 0 | postgres=2 project=18 | 3.64 | 0.5 | 0 % | 0.00 |
| 32 | 11067 | 2.75 | 7.19 | 0 | 32 | 110673 | 1 | postgres=2 project=18 | 3.65 | 0.5 | 0 % | 0.00 |
| 64 | 11888 | 5.19 | 12.03 | 0 | 64 | 118902 | 1 | postgres=2 project=18 | 3.81 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--yf906w0c as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 27820 | 0.03 | 0.08 | 0 | 0 | 277963 | 20043 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 0.50 |
| 4 | 54424 | 0.07 | 0.14 | 0 | 0 | 543598 | 39027 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 1.89 |
| 8 | 78746 | 0.08 | 0.27 | 0 | 0 | 786435 | 56143 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 3.09 |
| 16 | 77603 | 0.17 | 0.82 | 0 | 0 | 773208 | 55525 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 3.21 |
| 32 | 76465 | 0.31 | 1.93 | 0 | 0 | 758899 | 54673 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 3.26 |
| 64 | 75183 | 0.54 | 4.94 | 0 | 0 | 743212 | 53264 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 3.32 |
