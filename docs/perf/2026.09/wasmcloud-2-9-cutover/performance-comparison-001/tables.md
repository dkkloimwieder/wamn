## Repeated load measurements

The comparison uses 2.8 source `dfa1c3187fe8cd671688a442b23106046e502cb6` and 2.9 source `e7033f72ca32e92b02c3bbe1dc4af420cb23b145`.
RPS means requests per second.
P50 is the median request latency.
P99 is the 99th-percentile request latency.
Values below are medians across three sweeps, with throughput minimum and maximum values in brackets.
The sweeps share one host per source and do not represent independent deployments.
Host CPU time per request includes background work throughout the recorded counter window.

### Service credentials

| Concurrency | 2.8 RPS [min, max] | 2.9 RPS [min, max] | P50 ms, 2.8 to 2.9 | P99 ms, 2.8 to 2.9 | Host CPU ms/request, 2.8 to 2.9 |
|---|---|---|---|---|---|
| 1 | 358.55 [356.60, 381.55] | 332.85 [246.96, 384.64] | 2.581 to 2.844 | 6.514 to 6.076 | 4.601 to 5.030 |
| 4 | 1,014.96 [826.45, 1,046.36] | 1,090.47 [925.77, 1,117.05] | 3.502 to 3.410 | 11.440 to 7.525 | 3.023 to 2.979 |
| 8 | 1,381.50 [1,300.96, 1,431.44] | 1,413.83 [1,301.59, 1,513.51] | 5.411 to 5.175 | 11.700 to 13.046 | 2.704 to 2.679 |
| 16 | 1,500.22 [1,416.80, 1,536.44] | 1,526.09 [1,368.99, 1,568.66] | 10.076 to 9.988 | 24.434 to 19.508 | 2.699 to 2.681 |
| 32 | 1,205.01 [1,084.12, 1,347.78] | 1,198.97 [1,169.12, 1,330.49] | 25.396 to 25.236 | 50.155 to 48.085 | 2.903 to 2.882 |
| 64 | 1,362.56 [1,044.33, 1,434.67] | 1,166.72 [939.05, 1,317.58] | 47.165 to 52.728 | 86.023 to 92.816 | 2.776 to 2.956 |

### Human credentials

| Concurrency | 2.8 RPS [min, max] | 2.9 RPS [min, max] | P50 ms, 2.8 to 2.9 | P99 ms, 2.8 to 2.9 | Host CPU ms/request, 2.8 to 2.9 |
|---|---|---|---|---|---|
| 1 | 336.18 [318.32, 376.76] | 367.26 [352.55, 383.74] | 2.768 to 2.587 | 6.623 to 5.412 | 4.874 to 4.595 |
| 4 | 882.11 [606.55, 1,074.73] | 1,102.65 [930.43, 1,144.98] | 4.033 to 3.390 | 11.232 to 7.117 | 3.170 to 2.945 |
| 8 | 1,328.49 [999.37, 1,552.21] | 1,493.08 [1,002.65, 1,512.81] | 5.589 to 5.016 | 12.660 to 11.027 | 2.758 to 2.671 |
| 16 | 1,576.47 [1,442.09, 1,655.23] | 1,611.21 [1,507.30, 1,614.39] | 9.676 to 9.612 | 19.097 to 17.222 | 2.649 to 2.614 |
| 32 | 1,237.27 [1,210.45, 1,422.27] | 1,304.39 [1,285.45, 1,364.92] | 24.909 to 23.951 | 45.257 to 38.797 | 2.844 to 2.804 |
| 64 | 1,252.06 [1,189.38, 1,438.28] | 1,184.18 [1,080.18, 1,197.17] | 48.255 to 52.672 | 87.256 to 105.756 | 2.809 to 2.942 |

### Recorded request outcomes

| Source | Completed requests | Workload errors | Deadline cutoffs |
|---|---|---|---|
| dfa1c318 | 27,459,401 | 0 | 1,497 |
| e7033f72 | 26,077,227 | 0 | 1,499 |

Deadline cutoffs remain separate from workload errors and completed request counts.

The complete comparison retains every control layer, latency spread, native knee, ratio, CPU counter, and memory sample.
This table adds no acceptance rule or claim of statistical significance.
