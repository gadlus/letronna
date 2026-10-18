**85 tasks, 85 trials.** Mean reward 0.635; 54 passed every run.
The oracle failed 5 of these here (caffe-cifar-10, make-doom-for-mips, protein-assembly, qemu-alpine-ssh, qemu-startup); on the other 80, mean reward 0.650.
Errors: NonZeroAgentExitCodeError 1.
Per task: 975K tokens with cache reads, 16.7 min; cost $0.00 in all.

| Task | Runs | Reward | Oracle | Error | Tokens | Minutes |
|---|---|---|---|---|---|---|
| adaptive-rejection-sampler | 1 | 0.00 |  |  | 201K | 13.1 |
| bn-fit-modify | 1 | 1.00 |  |  | 119K | 6.3 |
| break-filter-js-from-html | 1 | 1.00 |  |  | 136K | 7.7 |
| build-cython-ext | 1 | 1.00 |  |  | 928K | 13.1 |
| build-pmars | 1 | 0.00 |  |  | 378K | 4.7 |
| build-pov-ray | 1 | 1.00 |  |  | 3145K | 19.8 |
| caffe-cifar-10 | 1 | 0.00 | 0 |  | 960K | 18.2 |
| cancel-async-tasks | 1 | 0.00 |  |  | 115K | 12.7 |
| chess-best-move | 1 | 0.00 |  |  | 1059K | 13.1 |
| circuit-fibsqrt | 1 | 1.00 |  |  | 1077K | 29.6 |
| cobol-modernization | 1 | 1.00 |  |  | 320K | 13.0 |
| code-from-image | 1 | 1.00 |  |  | 163K | 6.3 |
| compile-compcert | 1 | 0.00 |  |  | 670K | 16.3 |
| configure-git-webserver | 1 | 0.00 |  |  | 197K | 6.2 |
| constraints-scheduling | 1 | 1.00 |  |  | 380K | 12.0 |
| count-dataset-tokens | 1 | 1.00 |  |  | 355K | 7.8 |
| crack-7z-hash | 1 | 0.00 |  |  | 768K | 12.6 |
| custom-memory-heap-crash | 1 | 1.00 |  |  | 2024K | 19.7 |
| db-wal-recovery | 1 | 1.00 |  |  | 422K | 13.1 |
| distribution-search | 1 | 1.00 |  |  | 134K | 9.2 |
| dna-assembly | 1 | 1.00 |  |  | 1425K | 28.1 |
| dna-insert | 1 | 1.00 | 1 |  | 886K | 28.1 |
| extract-elf | 1 | 0.00 |  |  | 175K | 8.2 |
| extract-moves-from-video | 1 | 0.00 | 1 |  | 3122K | 28.2 |
| feal-differential-cryptanalysis | 1 | 1.00 |  |  | 1042K | 17.8 |
| feal-linear-cryptanalysis | 1 | 1.00 | 1 |  | 273K | 6.3 |
| filter-js-from-html | 1 | 0.00 |  |  | 900K | 28.1 |
| financial-document-processor | 1 | 1.00 |  |  | 517K | 8.3 |
| fix-code-vulnerability | 1 | 1.00 |  |  | 443K | 9.0 |
| fix-git | 1 | 1.00 |  |  | 102K | 2.6 |
| fix-ocaml-gc | 1 | 1.00 |  |  | 2693K | 47.7 |
| gcode-to-text | 1 | 0.00 | 1 |  | 990K | 12.6 |
| git-leak-recovery | 1 | 1.00 |  |  | 73K | 3.1 |
| git-multibranch | 1 | 1.00 |  |  | 899K | 13.1 |
| gpt2-codegolf | 1 | 0.00 | 1 |  | 601K | 13.3 |
| headless-terminal | 1 | 1.00 |  |  | 814K | 13.1 |
| install-windows-3.11 | 1 | 0.00 | 1 |  | 2524K | 58.1 |
| kv-store-grpc | 1 | 1.00 |  |  | 294K | 7.3 |
| large-scale-text-editing | 1 | 1.00 |  |  | 514K | 18.1 |
| largest-eigenval | 1 | 1.00 |  |  | 200K | 13.1 |
| llm-inference-batching-scheduler | 1 | 0.00 |  | NonZeroAgentExitCodeError | 79K | 5.4 |
| log-summary-date-ranges | 1 | 1.00 |  |  | 368K | 8.8 |
| mailman | 1 | 0.00 |  |  | 14K | 0.2 |
| make-doom-for-mips | 1 | 0.00 | 0 |  | 551K | 13.1 |
| make-mips-interpreter | 1 | 0.00 | 1 |  | 1198K | 28.1 |
| mcmc-sampling-stan | 1 | 0.00 |  |  | 1065K | 26.6 |
| merge-diff-arc-agi-task | 1 | 1.00 |  |  | 647K | 12.1 |
| model-extraction-relu-logits | 1 | 1.00 |  |  | 448K | 11.0 |
| modernize-scientific-stack | 1 | 1.00 |  |  | 295K | 8.1 |
| multi-source-data-merger | 1 | 1.00 |  |  | 252K | 8.0 |
| nginx-request-logging | 1 | 1.00 |  |  | 465K | 13.1 |
| openssl-selfsigned-cert | 1 | 1.00 |  |  | 393K | 8.8 |
| overfull-hbox | 1 | 1.00 | 1 |  | 584K | 8.3 |
| password-recovery | 1 | 1.00 |  |  | 518K | 11.2 |
| path-tracing | 1 | 1.00 |  |  | 2863K | 25.1 |
| path-tracing-reverse | 1 | 0.00 | 1 |  | 1987K | 24.1 |
| polyglot-c-py | 1 | 1.00 | 1 |  | 241K | 10.4 |
| polyglot-rust-c | 1 | 1.00 | 1 |  | 101K | 2.6 |
| portfolio-optimization | 1 | 1.00 |  |  | 832K | 41.4 |
| protein-assembly | 1 | 1.00 | 0 |  | 1473K | 28.0 |
| prove-plus-comm | 1 | 1.00 |  |  | 218K | 9.3 |
| pypi-server | 1 | 1.00 |  |  | 463K | 12.3 |
| pytorch-model-cli | 1 | 1.00 |  |  | 379K | 5.8 |
| pytorch-model-recovery | 1 | 1.00 |  |  | 580K | 13.0 |
| qemu-alpine-ssh | 1 | 0.00 | 0 |  | 693K | 13.0 |
| qemu-startup | 1 | 1.00 | 0 |  | 1233K | 13.0 |
| query-optimize | 1 | 1.00 | 1 |  | 462K | 9.2 |
| raman-fitting | 1 | 0.00 | 1 |  | 520K | 8.8 |
| regex-chess | 1 | 0.00 | 1 |  | 47K | 58.1 |
| regex-log | 1 | 1.00 |  |  | 330K | 6.4 |
| reshard-c4-data | 1 | 1.00 | 1 |  | 2397K | 34.8 |
| rstan-to-pystan | 1 | 1.00 |  |  | 1383K | 28.1 |
| sam-cell-seg | 1 | 0.00 | 1 |  | 3752K | 31.1 |
| sanitize-git-repo | 1 | 0.00 |  |  | 976K | 13.0 |
| schemelike-metacircular-eval | 1 | 1.00 |  |  | 3000K | 38.0 |
| sparql-university | 1 | 1.00 |  |  | 460K | 12.2 |
| sqlite-db-truncate | 1 | 1.00 |  |  | 162K | 5.3 |
| sqlite-with-gcov | 1 | 1.00 |  |  | 154K | 4.5 |
| torch-tensor-parallelism | 1 | 0.00 |  |  | 505K | 13.0 |
| train-fasttext | 1 | 0.00 | 1 |  | 573K | 39.1 |
| tune-mjcf | 1 | 0.00 | 1 |  | 337K | 11.3 |
| video-processing | 1 | 0.00 | 1 |  | 12421K | 58.0 |
| vulnerable-secret | 1 | 1.00 |  |  | 79K | 1.1 |
| winning-avg-corewars | 1 | 0.00 | 1 |  | 5305K | 57.5 |
| write-compressor | 1 | 0.00 | 1 |  | 14K | 13.0 |
