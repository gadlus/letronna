**89 tasks, 89 trials.** Mean reward 0.753; 67 passed every run.
The oracle failed 7 of these here (build-cython-ext, build-pmars, caffe-cifar-10, make-doom-for-mips, protein-assembly, qemu-alpine-ssh, qemu-startup); on the other 82, mean reward 0.756.
Errors: NonZeroAgentExitCodeError 1, VerifierTimeoutError 1.
Per task: 986K tokens with cache reads, 9.8 min; cost $0.00 in all.

| Task | Runs | Reward | Oracle | Error | Tokens | Minutes |
|---|---|---|---|---|---|---|
| adaptive-rejection-sampler | 1 | 0.00 | 1 |  | 308K | 13.0 |
| bn-fit-modify | 1 | 1.00 | 1 |  | 426K | 10.4 |
| break-filter-js-from-html | 1 | 1.00 | 1 |  | 243K | 3.7 |
| build-cython-ext | 1 | 1.00 | 0 |  | 1130K | 8.4 |
| build-pmars | 1 | 1.00 | 0 |  | 230K | 1.8 |
| build-pov-ray | 1 | 1.00 | 1 |  | 1374K | 6.4 |
| caffe-cifar-10 | 1 | 0.00 | 0 |  | 190K | 18.0 |
| cancel-async-tasks | 1 | 1.00 | 1 |  | 270K | 4.2 |
| chess-best-move | 1 | 1.00 | 1 |  | 205K | 2.2 |
| circuit-fibsqrt | 1 | 0.00 | 1 |  | 73K | 58.0 |
| cobol-modernization | 1 | 1.00 | 1 |  | 977K | 6.4 |
| code-from-image | 1 | 1.00 | 1 |  | 19K | 0.2 |
| compile-compcert | 1 | 1.00 |  |  | 1025K | 17.8 |
| configure-git-webserver | 1 | 1.00 | 1 |  | 415K | 3.5 |
| constraints-scheduling | 1 | 1.00 | 1 |  | 51K | 0.6 |
| count-dataset-tokens | 1 | 1.00 | 1 |  | 290K | 2.7 |
| crack-7z-hash | 1 | 1.00 | 1 |  | 252K | 3.5 |
| custom-memory-heap-crash | 1 | 1.00 |  |  | 907K | 4.7 |
| db-wal-recovery | 1 | 1.00 |  |  | 149K | 2.1 |
| distribution-search | 1 | 1.00 |  |  | 83K | 2.6 |
| dna-assembly | 1 | 1.00 |  |  | 3208K | 16.1 |
| dna-insert | 1 | 0.00 | 1 |  | 337K | 9.9 |
| extract-elf | 1 | 0.00 |  |  | 345K | 4.6 |
| extract-moves-from-video | 1 | 0.00 | 1 |  | 3176K | 26.3 |
| feal-differential-cryptanalysis | 1 | 1.00 |  |  | 1279K | 19.1 |
| feal-linear-cryptanalysis | 1 | 1.00 | 1 |  | 460K | 9.6 |
| filter-js-from-html | 1 | 0.00 |  | VerifierTimeoutError | 471K | 8.0 |
| financial-document-processor | 1 | 1.00 |  |  | 600K | 5.9 |
| fix-code-vulnerability | 1 | 1.00 |  |  | 96K | 0.7 |
| fix-git | 1 | 1.00 |  |  | 158K | 1.6 |
| fix-ocaml-gc | 1 | 1.00 |  |  | 3656K | 19.0 |
| gcode-to-text | 1 | 0.00 | 1 |  | 640K | 5.9 |
| git-leak-recovery | 1 | 1.00 |  |  | 72K | 0.9 |
| git-multibranch | 1 | 1.00 |  |  | 533K | 6.3 |
| gpt2-codegolf | 1 | 0.00 | 1 |  | 1033K | 13.0 |
| headless-terminal | 1 | 1.00 |  |  | 443K | 6.1 |
| hf-model-inference | 1 | 1.00 |  |  | 141K | 2.9 |
| install-windows-3.11 | 1 | 0.00 | 1 |  | 6806K | 31.5 |
| kv-store-grpc | 1 | 1.00 |  |  | 108K | 1.3 |
| large-scale-text-editing | 1 | 1.00 |  |  | 102K | 2.0 |
| largest-eigenval | 1 | 1.00 |  |  | 1497K | 9.7 |
| llm-inference-batching-scheduler | 1 | 1.00 |  |  | 1759K | 9.5 |
| log-summary-date-ranges | 1 | 1.00 |  |  | 79K | 0.7 |
| mailman | 1 | 1.00 |  |  | 2672K | 10.4 |
| make-doom-for-mips | 1 | 0.00 | 0 |  | 3669K | 11.9 |
| make-mips-interpreter | 1 | 1.00 | 1 |  | 6481K | 19.7 |
| mcmc-sampling-stan | 1 | 1.00 |  |  | 509K | 16.1 |
| merge-diff-arc-agi-task | 1 | 0.00 |  | NonZeroAgentExitCodeError | 94K | 3.4 |
| model-extraction-relu-logits | 1 | 1.00 |  |  | 316K | 3.2 |
| modernize-scientific-stack | 1 | 1.00 |  |  | 53K | 0.6 |
| mteb-leaderboard | 1 | 1.00 |  |  | 901K | 26.9 |
| mteb-retrieve | 1 | 1.00 |  |  | 430K | 5.3 |
| multi-source-data-merger | 1 | 1.00 |  |  | 71K | 1.0 |
| nginx-request-logging | 1 | 1.00 |  |  | 130K | 1.5 |
| openssl-selfsigned-cert | 1 | 1.00 |  |  | 67K | 0.6 |
| overfull-hbox | 1 | 1.00 | 1 |  | 330K | 2.3 |
| password-recovery | 1 | 1.00 |  |  | 360K | 2.9 |
| path-tracing | 1 | 1.00 |  |  | 932K | 5.2 |
| path-tracing-reverse | 1 | 1.00 | 1 |  | 3322K | 10.4 |
| polyglot-c-py | 1 | 0.00 | 1 |  | 393K | 7.6 |
| polyglot-rust-c | 1 | 1.00 | 1 |  | 157K | 3.6 |
| portfolio-optimization | 1 | 1.00 |  |  | 660K | 9.4 |
| protein-assembly | 1 | 1.00 | 0 |  | 706K | 5.8 |
| prove-plus-comm | 1 | 1.00 |  |  | 52K | 0.9 |
| pypi-server | 1 | 0.00 |  |  | 114K | 1.2 |
| pytorch-model-cli | 1 | 0.00 |  |  | 698K | 4.1 |
| pytorch-model-recovery | 1 | 1.00 |  |  | 172K | 3.5 |
| qemu-alpine-ssh | 1 | 1.00 | 0 |  | 363K | 4.9 |
| qemu-startup | 1 | 1.00 | 0 |  | 119K | 2.7 |
| query-optimize | 1 | 0.00 | 1 |  | 128K | 5.8 |
| raman-fitting | 1 | 1.00 | 1 |  | 972K | 6.6 |
| regex-chess | 1 | 0.00 | 1 |  | 95K | 58.0 |
| regex-log | 1 | 1.00 |  |  | 132K | 1.9 |
| reshard-c4-data | 1 | 1.00 | 1 |  | 2598K | 12.3 |
| rstan-to-pystan | 1 | 1.00 |  |  | 489K | 9.5 |
| sam-cell-seg | 1 | 0.00 | 1 |  | 5940K | 39.2 |
| sanitize-git-repo | 1 | 0.00 |  |  | 646K | 5.1 |
| schemelike-metacircular-eval | 1 | 1.00 |  |  | 2605K | 37.7 |
| sparql-university | 1 | 1.00 |  |  | 174K | 1.9 |
| sqlite-db-truncate | 1 | 1.00 |  |  | 150K | 2.2 |
| sqlite-with-gcov | 1 | 1.00 |  |  | 109K | 1.6 |
| torch-pipeline-parallelism | 1 | 1.00 |  |  | 2116K | 10.9 |
| torch-tensor-parallelism | 1 | 1.00 |  |  | 159K | 3.6 |
| train-fasttext | 1 | 0.00 | 1 |  | 965K | 58.0 |
| tune-mjcf | 1 | 1.00 | 1 |  | 622K | 7.1 |
| video-processing | 1 | 1.00 | 1 |  | 5666K | 28.2 |
| vulnerable-secret | 1 | 1.00 |  |  | 84K | 0.5 |
| winning-avg-corewars | 1 | 0.00 | 1 |  | 4663K | 21.7 |
| write-compressor | 1 | 0.00 | 1 |  | 60K | 13.0 |
