**pass@1 = 58/89 = 65.17%**

- 95% CI (bootstrap, seed 20260729, 50,000 draws): [55.06%, 75.28%]
- 95% CI (Clopper–Pearson exact): [54.33%, 74.96%]
- spend: $75.83 (reported by 89/89 trials) · tokens: 353,694,196 in / 5,164,527 out
- trials with an exception: 45 · trials with zero tool calls: 0 (of 89/89 reporting)

| task | reward | tool calls | USD |
|---|---:|---:|---:|
| `terminal-bench/adaptive-rejection-sampler` | 1.0 | 69 | 0.8201 |
| `terminal-bench/bn-fit-modify` | 1.0 | 34 | 0.3218 |
| `terminal-bench/break-filter-js-from-html` | 1.0 | 40 | 0.1967 |
| `terminal-bench/build-cython-ext` | 1.0 | 130 | 2.2243 |
| `terminal-bench/build-pmars` | 1.0 | 98 | 0.6028 |
| `terminal-bench/build-pov-ray` | 0.0 | 108 | 1.1781 |
| `terminal-bench/caffe-cifar-10` | 1.0 | 104 | 0.7028 |
| `terminal-bench/cancel-async-tasks` | 0.0 | 21 | 0.1172 |
| `terminal-bench/chess-best-move` | 1.0 | 18 | 0.1448 |
| `terminal-bench/circuit-fibsqrt` | 1.0 | 123 | 2.2888 |
| `terminal-bench/cobol-modernization` | 1.0 | 98 | 0.8297 |
| `terminal-bench/code-from-image` | 1.0 | 55 | 1.0116 |
| `terminal-bench/compile-compcert` | 1.0 | 102 | 0.5845 |
| `terminal-bench/configure-git-webserver` | 0.0 | 16 | 0.0361 |
| `terminal-bench/constraints-scheduling` | 0.0 | 102 | 1.4214 |
| `terminal-bench/count-dataset-tokens` | 1.0 | 19 | 0.0924 |
| `terminal-bench/crack-7z-hash` | 0.0 | 43 | 0.1167 |
| `terminal-bench/custom-memory-heap-crash` | 1.0 | 196 | 2.2148 |
| `terminal-bench/db-wal-recovery` | 1.0 | 20 | 0.1039 |
| `terminal-bench/distribution-search` | 1.0 | 4 | 0.0575 |
| `terminal-bench/dna-assembly` | 0.0 | 134 | 5.1027 |
| `terminal-bench/dna-insert` | 1.0 | 113 | 1.8203 |
| `terminal-bench/extract-elf` | 0.0 | 11 | 0.0925 |
| `terminal-bench/extract-moves-from-video` | 0.0 | 144 | 1.0586 |
| `terminal-bench/feal-differential-cryptanalysis` | 1.0 | 13 | 0.1339 |
| `terminal-bench/feal-linear-cryptanalysis` | 1.0 | 82 | 1.9095 |
| `terminal-bench/filter-js-from-html` | 0.0 | 58 | 0.2665 |
| `terminal-bench/financial-document-processor` | 0.0 | 20 | 0.1908 |
| `terminal-bench/fix-code-vulnerability` | 0.0 | 88 | 0.6113 |
| `terminal-bench/fix-git` | 0.0 | 11 | 0.0261 |
| `terminal-bench/fix-ocaml-gc` | 1.0 | 130 | 2.2053 |
| `terminal-bench/gcode-to-text` | 0.0 | 88 | 1.3849 |
| `terminal-bench/git-leak-recovery` | 1.0 | 107 | 0.8975 |
| `terminal-bench/git-multibranch` | 1.0 | 50 | 0.3986 |
| `terminal-bench/gpt2-codegolf` | 0.0 | 6 | 0.1658 |
| `terminal-bench/headless-terminal` | 1.0 | 33 | 0.3200 |
| `terminal-bench/hf-model-inference` | 1.0 | 71 | 0.2510 |
| `terminal-bench/install-windows-3.11` | 1.0 | 94 | 0.7382 |
| `terminal-bench/kv-store-grpc` | 0.0 | 17 | 0.0571 |
| `terminal-bench/large-scale-text-editing` | 1.0 | 47 | 0.2426 |
| `terminal-bench/largest-eigenval` | 1.0 | 138 | 1.0704 |
| `terminal-bench/llm-inference-batching-scheduler` | 1.0 | 106 | 2.7459 |
| `terminal-bench/log-summary-date-ranges` | 1.0 | 7 | 0.0378 |
| `terminal-bench/mailman` | 1.0 | 151 | 2.0509 |
| `terminal-bench/make-doom-for-mips` | 0.0 | 80 | 0.8932 |
| `terminal-bench/make-mips-interpreter` | 1.0 | 70 | 1.6887 |
| `terminal-bench/mcmc-sampling-stan` | 1.0 | 59 | 0.4174 |
| `terminal-bench/merge-diff-arc-agi-task` | 1.0 | 31 | 0.3136 |
| `terminal-bench/model-extraction-relu-logits` | 1.0 | 32 | 0.7355 |
| `terminal-bench/modernize-scientific-stack` | 1.0 | 16 | 0.0410 |
| `terminal-bench/mteb-leaderboard` | 0.0 | 39 | 0.3497 |
| `terminal-bench/mteb-retrieve` | 0.0 | 23 | 0.0909 |
| `terminal-bench/multi-source-data-merger` | 1.0 | 49 | 0.4336 |
| `terminal-bench/nginx-request-logging` | 1.0 | 28 | 0.0900 |
| `terminal-bench/openssl-selfsigned-cert` | 0.0 | 26 | 0.0632 |
| `terminal-bench/overfull-hbox` | 1.0 | 52 | 0.3358 |
| `terminal-bench/password-recovery` | 1.0 | 29 | 0.1192 |
| `terminal-bench/path-tracing` | 1.0 | 120 | 1.9744 |
| `terminal-bench/path-tracing-reverse` | 0.0 | 25 | 0.3728 |
| `terminal-bench/polyglot-c-py` | 1.0 | 12 | 0.0995 |
| `terminal-bench/polyglot-rust-c` | 1.0 | 4 | 0.1689 |
| `terminal-bench/portfolio-optimization` | 1.0 | 35 | 0.3392 |
| `terminal-bench/protein-assembly` | 0.0 | 47 | 1.8502 |
| `terminal-bench/prove-plus-comm` | 1.0 | 8 | 0.0167 |
| `terminal-bench/pypi-server` | 1.0 | 55 | 0.2030 |
| `terminal-bench/pytorch-model-cli` | 0.0 | 15 | 0.1376 |
| `terminal-bench/pytorch-model-recovery` | 0.0 | 53 | 0.9542 |
| `terminal-bench/qemu-alpine-ssh` | 1.0 | 115 | 0.5034 |
| `terminal-bench/qemu-startup` | 1.0 | 17 | 0.0422 |
| `terminal-bench/query-optimize` | 0.0 | 35 | 0.1553 |
| `terminal-bench/raman-fitting` | 0.0 | 50 | 0.6170 |
| `terminal-bench/regex-chess` | 1.0 | 230 | 5.3173 |
| `terminal-bench/regex-log` | 1.0 | 13 | 0.1094 |
| `terminal-bench/reshard-c4-data` | 1.0 | 254 | 3.3348 |
| `terminal-bench/rstan-to-pystan` | 0.0 | 42 | 0.2154 |
| `terminal-bench/sam-cell-seg` | 1.0 | 171 | 2.1232 |
| `terminal-bench/sanitize-git-repo` | 0.0 | 36 | 0.0687 |
| `terminal-bench/schemelike-metacircular-eval` | 1.0 | 98 | 1.7822 |
| `terminal-bench/sparql-university` | 1.0 | 12 | 0.1136 |
| `terminal-bench/sqlite-db-truncate` | 1.0 | 10 | 0.0587 |
| `terminal-bench/sqlite-with-gcov` | 1.0 | 22 | 0.1186 |
| `terminal-bench/torch-pipeline-parallelism` | 0.0 | 7 | 0.0309 |
| `terminal-bench/torch-tensor-parallelism` | 1.0 | 30 | 0.9761 |
| `terminal-bench/train-fasttext` | 0.0 | 136 | 0.8164 |
| `terminal-bench/tune-mjcf` | 1.0 | 41 | 0.3710 |
| `terminal-bench/video-processing` | 0.0 | 125 | 3.4970 |
| `terminal-bench/vulnerable-secret` | 1.0 | 22 | 0.0996 |
| `terminal-bench/winning-avg-corewars` | 1.0 | 177 | 4.7910 |
| `terminal-bench/write-compressor` | 0.0 | 3 | 0.1582 |
