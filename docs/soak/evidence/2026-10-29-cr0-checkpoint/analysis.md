# CR-0 soak evidence

Commit: `744b84d68c7609d8220430ffc9938c05c6d6a527`.
Binary SHA-256: `24CB1EF43D86AD8C0F9F80C41E9D2D661F3FD6FE5573E72C13A87285E11E1475`.

Run: completion_unknown. Baseline: completion_unknown.

Stop reason: not recorded. Clean shutdown: not recorded.

| Phase | Samples | Span (h) | Covered (h) | Health failures | Metrics failures | Peak RSS (MiB) | RSS change (MiB/h) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| startup | 357 | 6.00 | 6.00 | 0 | 0 | 52.69 | -0.98 |
| warmup | 30 | 0.49 | 0.49 | 0 | 0 | 324.04 | 403.57 |
| baseline | 960 | 16.13 | 16.13 | 0 | 0 | 535.99 | -10.07 |

Observation freshness: fresh_observations.
Baseline valid coverage: 67.21%.
Disk summary: {"observations": 8075, "read_failure_observations": 0, "minimum_free_bytes": 64973492224, "max_redirected_log_bytes": 367873, "hourly": {"0": {"observations": 357, "read_failure_observations": 0, "minimum_free_bytes": 124532953088, "max_redirected_log_bytes": 5757}, "1": {"observations": 356, "read_failure_observations": 0, "minimum_free_bytes": 124523479040, "max_redirected_log_bytes": 9261}, "2": {"observations": 356, "read_failure_observations": 0, "minimum_free_bytes": 124066689024, "max_redirected_log_bytes": 12327}, "3": {"observations": 356, "read_failure_observations": 0, "minimum_free_bytes": 124622999552, "max_redirected_log_bytes": 16025}, "4": {"observations": 356, "read_failure_observations": 0, "minimum_free_bytes": 124580454400, "max_redirected_log_bytes": 20064}, "5": {"observations": 356, "read_failure_observations": 0, "minimum_free_bytes": 124544786432, "max_redirected_log_bytes": 24833}, "6": {"observations": 357, "read_failure_observations": 0, "minimum_free_bytes": 124316073984, "max_redirected_log_bytes": 45479}, "7": {"observations": 357, "read_failure_observations": 0, "minimum_free_bytes": 123871465472, "max_redirected_log_bytes": 66128}, "8": {"observations": 356, "read_failure_observations": 0, "minimum_free_bytes": 123511447552, "max_redirected_log_bytes": 86639}, "9": {"observations": 357, "read_failure_observations": 0, "minimum_free_bytes": 123077586944, "max_redirected_log_bytes": 107287}, "10": {"observations": 356, "read_failure_observations": 0, "minimum_free_bytes": 122621927424, "max_redirected_log_bytes": 127784}, "11": {"observations": 357, "read_failure_observations": 0, "minimum_free_bytes": 122169790464, "max_redirected_log_bytes": 148432}, "12": {"observations": 357, "read_failure_observations": 0, "minimum_free_bytes": 121270247424, "max_redirected_log_bytes": 169086}, "13": {"observations": 356, "read_failure_observations": 0, "minimum_free_bytes": 120410480640, "max_redirected_log_bytes": 189729}, "14": {"observations": 357, "read_failure_observations": 0, "minimum_free_bytes": 119418314752, "max_redirected_log_bytes": 210223}, "15": {"observations": 357, "read_failure_observations": 0, "minimum_free_bytes": 93505302528, "max_redirected_log_bytes": 230864}, "16": {"observations": 354, "read_failure_observations": 0, "minimum_free_bytes": 95534764032, "max_redirected_log_bytes": 251359}, "17": {"observations": 357, "read_failure_observations": 0, "minimum_free_bytes": 88554065920, "max_redirected_log_bytes": 272001}, "18": {"observations": 357, "read_failure_observations": 0, "minimum_free_bytes": 80650272768, "max_redirected_log_bytes": 292818}, "19": {"observations": 356, "read_failure_observations": 0, "minimum_free_bytes": 73742995456, "max_redirected_log_bytes": 313317}, "20": {"observations": 358, "read_failure_observations": 0, "minimum_free_bytes": 70903386112, "max_redirected_log_bytes": 333960}, "21": {"observations": 357, "read_failure_observations": 0, "minimum_free_bytes": 66186072064, "max_redirected_log_bytes": 354600}, "22": {"observations": 232, "read_failure_observations": 0, "minimum_free_bytes": 64973492224, "max_redirected_log_bytes": 367873}}}

## Evidence issues

No collection issues detected in this snapshot.

## Startup

| Counter | Observed delta | Per hour | Covered seconds |
| --- | ---: | ---: | ---: |
| dht_peer_admitted_total | 304.00 | 50.68 | 21,594.30 |
| dht_peer_rejected_total | 0.00 | 0.00 | 21,594.30 |
| dht_peer_cancelled_total | 0.00 | 0.00 | 21,594.30 |
| dht_peer_rejection_reasons_total | 0.00 | 0.00 | 21,594.30 |
| dht_peer_ack_failed_total | 0.00 | 0.00 | 21,594.30 |
| dht_peer_command_full_total | 0.00 | 0.00 | 21,594.30 |
| dht_peer_command_closed_total | 0.00 | 0.00 | 21,594.30 |
| dht_lookups_completed_total | 304.00 | 50.68 | 21,594.30 |
| dht_lookup_deadlines_total | 2,718.00 | 453.12 | 21,594.30 |
| dht_sample_stale_total | 0.00 | 0.00 | 21,594.30 |
| dht_sample_deferred_total | 0.00 | 0.00 | 21,594.30 |
| dht_queries_out_total | 86,924.00 | 14,491.16 | 21,594.30 |
| dht_responses_in_total | 28,531.00 | 4,756.42 | 21,594.30 |
| dht_timeouts_total | 44,391.00 | 7,400.45 | 21,594.30 |
| dht_packets_in_total | 38,281.00 | 6,381.85 | 21,594.30 |
| dht_packets_out_total | 87,595.00 | 14,603.02 | 21,594.30 |
| dht_harvested_total | 80.00 | 13.34 | 21,594.30 |
| dht_harvest_dropped_total | 0.00 | 0.00 | 21,594.30 |
| fetch_attempts_total | 8.00 | 1.33 | 21,594.30 |
| fetch_success_total | 0.00 | 0.00 | 21,594.30 |
| fetch_failures_total | 8.00 | 1.33 | 21,594.30 |
| fetch_lost_total | 68.00 | 11.34 | 21,594.30 |
| fetch_finder_full_total | 0.00 | 0.00 | 21,594.30 |
| fetch_finder_closed_total | 0.00 | 0.00 | 21,594.30 |
| fetch_lookup_unaccepted_total | 0.00 | 0.00 | 21,594.30 |
| fetch_retry_unaccepted_total | 0.00 | 0.00 | 21,594.30 |
| fetch_evicted_service_total | 0.00 | 0.00 | 21,594.30 |
| writer_stored_total | 0.00 | 0.00 | 21,594.30 |
| writer_errors_total | 0.00 | 0.00 | 21,594.30 |
| writer_transactions_total | 129.00 | 21.51 | 21,594.30 |
| search_errors_total | 0.00 | 0.00 | 21,594.30 |

Reason breakdown: fetch_evicted_service_total

| Reason | Delta | Covered seconds |
| --- | ---: | ---: |
| fetch | 0.00 | 21,594.30 |
| lookup | 0.00 | 21,594.30 |
| peers | 0.00 | 21,594.30 |
| unaccepted | 0.00 | 21,594.30 |

Reason breakdown: fetch_failures_total

| Reason | Delta | Covered seconds |
| --- | ---: | ---: |
| encryption_failed | 0.00 | 21,594.30 |
| invalid | 0.00 | 21,594.30 |
| oversize | 0.00 | 21,594.30 |
| refused | 8.00 | 21,594.30 |
| timed_out | 0.00 | 21,594.30 |
| unreachable | 0.00 | 21,594.30 |

Reason breakdown: dht_peer_rejection_reasons_total

| Reason | Delta | Covered seconds |
| --- | ---: | ---: |
| caller_closed | 0.00 | 21,594.30 |
| capacity | 0.00 | 21,594.30 |
| paused | 0.00 | 21,594.30 |

Reason breakdown: fetch_lost_total

| Reason | Delta | Covered seconds |
| --- | ---: | ---: |
| blocked | 0.00 | 21,594.30 |
| encryption_failed | 0.00 | 21,594.30 |
| evicted | 0.00 | 21,594.30 |
| gave_up | 0.00 | 21,594.30 |
| invalid | 0.00 | 21,594.30 |
| no_peers | 62.00 | 21,594.30 |
| oversize | 0.00 | 21,594.30 |
| refused | 6.00 | 21,594.30 |
| timed_out | 0.00 | 21,594.30 |
| unreachable | 0.00 | 21,594.30 |

| Resource | Observed max | Limit | At capacity samples | Over capacity samples |
| --- | ---: | ---: | ---: | ---: |
| peer_requests | 5.00 | 32 | 0 | 0 |
| sampler_states | 0.00 | 200000 | 0 | 0 |
| sample_frontier | 20,000.00 | 20000 | 43 | 0 |
| outstanding_rpcs | 56.00 | 1024 | 0 | 0 |
| pool_lookups | 5.00 | 256 | 0 | 0 |

## Warmup

| Counter | Observed delta | Per hour | Covered seconds |
| --- | ---: | ---: | ---: |
| dht_peer_admitted_total | 2,221.00 | 4,555.64 | 1,755.10 |
| dht_peer_rejected_total | 2,693,201.00 | 5,524,200.10 | 1,755.10 |
| dht_peer_cancelled_total | 5.00 | 10.26 | 1,755.10 |
| dht_peer_rejection_reasons_total | 2,693,201.00 | 5,524,200.10 | 1,755.10 |
| dht_peer_ack_failed_total | 0.00 | 0.00 | 1,755.10 |
| dht_peer_command_full_total | 483,540.00 | 991,820.41 | 1,755.10 |
| dht_peer_command_closed_total | 0.00 | 0.00 | 1,755.10 |
| dht_lookups_completed_total | 2,219.00 | 4,551.54 | 1,755.10 |
| dht_lookup_deadlines_total | 1,996.00 | 4,094.13 | 1,755.10 |
| dht_sample_stale_total | 0.00 | 0.00 | 1,755.10 |
| dht_sample_deferred_total | 0.00 | 0.00 | 1,755.10 |
| dht_queries_out_total | 274,132.00 | 562,290.01 | 1,755.10 |
| dht_responses_in_total | 147,464.00 | 302,473.02 | 1,755.10 |
| dht_timeouts_total | 117,549.00 | 241,112.42 | 1,755.10 |
| dht_packets_in_total | 161,987.00 | 332,262.09 | 1,755.10 |
| dht_packets_out_total | 275,877.00 | 565,869.30 | 1,755.10 |
| dht_harvested_total | 1,100,870.00 | 2,258,066.21 | 1,755.10 |
| dht_harvest_dropped_total | 0.00 | 0.00 | 1,755.10 |
| fetch_attempts_total | 12,858.00 | 26,373.88 | 1,755.10 |
| fetch_success_total | 573.00 | 1,175.32 | 1,755.10 |
| fetch_failures_total | 12,286.00 | 25,200.62 | 1,755.10 |
| fetch_lost_total | 921,376.00 | 1,889,894.36 | 1,755.10 |
| fetch_finder_full_total | 0.00 | 0.00 | 1,755.10 |
| fetch_finder_closed_total | 0.00 | 0.00 | 1,755.10 |
| fetch_lookup_unaccepted_total | 3,177,739.00 | 6,518,067.57 | 1,755.10 |
| fetch_retry_unaccepted_total | 2,076,771.00 | 4,259,800.35 | 1,755.10 |
| fetch_evicted_service_total | 921,376.00 | 1,889,894.36 | 1,755.10 |
| writer_stored_total | 571.00 | 1,171.22 | 1,755.10 |
| writer_errors_total | 0.00 | 0.00 | 1,755.10 |
| writer_transactions_total | 982.00 | 2,014.24 | 1,755.10 |
| search_errors_total | 0.00 | 0.00 | 1,755.10 |

Reason breakdown: fetch_evicted_service_total

| Reason | Delta | Covered seconds |
| --- | ---: | ---: |
| fetch | 1,019.00 | 1,755.10 |
| lookup | 294.00 | 1,755.10 |
| peers | 0.00 | 1,755.10 |
| unaccepted | 920,063.00 | 1,755.10 |

Reason breakdown: fetch_failures_total

| Reason | Delta | Covered seconds |
| --- | ---: | ---: |
| encryption_failed | 0.00 | 1,755.10 |
| invalid | 10.00 | 1,755.10 |
| oversize | 0.00 | 1,755.10 |
| refused | 1,464.00 | 1,755.10 |
| timed_out | 9,101.00 | 1,755.10 |
| unreachable | 1,711.00 | 1,755.10 |

Reason breakdown: dht_peer_rejection_reasons_total

| Reason | Delta | Covered seconds |
| --- | ---: | ---: |
| caller_closed | 0.00 | 1,755.10 |
| capacity | 2,693,201.00 | 1,755.10 |
| paused | 0.00 | 1,755.10 |

Reason breakdown: fetch_lost_total

| Reason | Delta | Covered seconds |
| --- | ---: | ---: |
| blocked | 0.00 | 1,755.10 |
| encryption_failed | 0.00 | 1,755.10 |
| evicted | 921,376.00 | 1,755.10 |
| gave_up | 0.00 | 1,755.10 |
| invalid | 0.00 | 1,755.10 |
| no_peers | 0.00 | 1,755.10 |
| oversize | 0.00 | 1,755.10 |
| refused | 0.00 | 1,755.10 |
| timed_out | 0.00 | 1,755.10 |
| unreachable | 0.00 | 1,755.10 |

| Resource | Observed max | Limit | At capacity samples | Over capacity samples |
| --- | ---: | ---: | ---: | ---: |
| peer_requests | 32.00 | 32 | 5 | 0 |
| sampler_states | 178,364.00 | 200000 | 0 | 0 |
| sample_frontier | 19,900.00 | 20000 | 0 | 0 |
| outstanding_rpcs | 383.00 | 1024 | 0 | 0 |
| pool_lookups | 106.00 | 256 | 0 | 0 |

## Baseline

| Counter | Observed delta | Per hour | Covered seconds |
| --- | ---: | ---: | ---: |
| dht_peer_admitted_total | 74,902.00 | 4,643.64 | 58,068.10 |
| dht_peer_rejected_total | 63,697,589.00 | 3,949,006.78 | 58,068.10 |
| dht_peer_cancelled_total | 361.00 | 22.38 | 58,068.10 |
| dht_peer_rejection_reasons_total | 63,697,589.00 | 3,949,006.78 | 58,068.10 |
| dht_peer_ack_failed_total | 0.00 | 0.00 | 58,068.10 |
| dht_peer_command_full_total | 32,754,009.00 | 2,030,623.22 | 58,068.10 |
| dht_peer_command_closed_total | 0.00 | 0.00 | 58,068.10 |
| dht_lookups_completed_total | 74,880.00 | 4,642.27 | 58,068.10 |
| dht_lookup_deadlines_total | 72,802.00 | 4,513.45 | 58,068.10 |
| dht_sample_stale_total | 0.00 | 0.00 | 58,068.10 |
| dht_sample_deferred_total | 52,988.00 | 3,285.05 | 58,068.10 |
| dht_queries_out_total | 4,067,790.00 | 252,187.41 | 58,068.10 |
| dht_responses_in_total | 2,310,136.00 | 143,219.59 | 58,068.10 |
| dht_timeouts_total | 1,532,396.00 | 95,002.69 | 58,068.10 |
| dht_packets_in_total | 2,623,266.00 | 162,632.45 | 58,068.10 |
| dht_packets_out_total | 4,116,145.00 | 255,185.24 | 58,068.10 |
| dht_harvested_total | 2,221,472.00 | 137,722.76 | 58,068.10 |
| dht_harvest_dropped_total | 0.00 | 0.00 | 58,068.10 |
| fetch_attempts_total | 270,342.00 | 16,760.17 | 58,068.10 |
| fetch_success_total | 11,606.00 | 719.53 | 58,068.10 |
| fetch_failures_total | 258,745.00 | 16,041.20 | 58,068.10 |
| fetch_lost_total | 2,210,501.00 | 137,042.60 | 58,068.10 |
| fetch_finder_full_total | 0.00 | 0.00 | 58,068.10 |
| fetch_finder_closed_total | 0.00 | 0.00 | 58,068.10 |
| fetch_lookup_unaccepted_total | 96,451,059.00 | 5,979,596.58 | 58,068.10 |
| fetch_retry_unaccepted_total | 92,838,608.00 | 5,755,638.45 | 58,068.10 |
| fetch_evicted_service_total | 2,209,749.00 | 136,995.98 | 58,068.10 |
| writer_stored_total | 11,574.00 | 717.54 | 58,068.10 |
| writer_errors_total | 0.00 | 0.00 | 58,068.10 |
| writer_transactions_total | 26,438.00 | 1,639.05 | 58,068.10 |
| search_errors_total | 0.00 | 0.00 | 58,068.10 |

Reason breakdown: fetch_evicted_service_total

| Reason | Delta | Covered seconds |
| --- | ---: | ---: |
| fetch | 19,564.00 | 58,068.10 |
| lookup | 4,828.00 | 58,068.10 |
| peers | 0.00 | 58,068.10 |
| unaccepted | 2,185,357.00 | 58,068.10 |

Reason breakdown: fetch_failures_total

| Reason | Delta | Covered seconds |
| --- | ---: | ---: |
| encryption_failed | 0.00 | 58,068.10 |
| invalid | 74.00 | 58,068.10 |
| oversize | 6.00 | 58,068.10 |
| refused | 28,965.00 | 58,068.10 |
| timed_out | 192,613.00 | 58,068.10 |
| unreachable | 37,087.00 | 58,068.10 |

Reason breakdown: dht_peer_rejection_reasons_total

| Reason | Delta | Covered seconds |
| --- | ---: | ---: |
| caller_closed | 22.00 | 58,068.10 |
| capacity | 63,697,567.00 | 58,068.10 |
| paused | 0.00 | 58,068.10 |

Reason breakdown: fetch_lost_total

| Reason | Delta | Covered seconds |
| --- | ---: | ---: |
| blocked | 0.00 | 58,068.10 |
| encryption_failed | 0.00 | 58,068.10 |
| evicted | 2,209,749.00 | 58,068.10 |
| gave_up | 0.00 | 58,068.10 |
| invalid | 0.00 | 58,068.10 |
| no_peers | 73.00 | 58,068.10 |
| oversize | 6.00 | 58,068.10 |
| refused | 187.00 | 58,068.10 |
| timed_out | 426.00 | 58,068.10 |
| unreachable | 60.00 | 58,068.10 |

| Resource | Observed max | Limit | At capacity samples | Over capacity samples |
| --- | ---: | ---: | ---: | ---: |
| peer_requests | 32.00 | 32 | 296 | 0 |
| sampler_states | 200,000.00 | 200000 | 885 | 0 |
| sample_frontier | 20,000.00 | 20000 | 885 | 0 |
| outstanding_rpcs | 371.00 | 1024 | 0 | 0 |
| pool_lookups | 256.00 | 256 | 26 | 0 |

## Interpretation

- Review evidence; this report never declares soak acceptance or a performance improvement.
- Rates use adjacent same-phase, same-process samples within the gap limit; coverage is per counter.
- Health failures remain counted and do not remove valid metric intervals or unsuccessful work.
- Counter resets or changed label sets invalidate that counter interval; absent data is unknown.
- Stored torrents are not verified searchable yield; no cohort latency or kernel-drop proof is available.
- Capacity equality is backpressure; over-capacity observations require review.
- RSS change is first-to-last within a continuous phase, not evidence of a leak.
- Final shutdown metrics have no monotonic sample timestamp and are excluded from rates.
