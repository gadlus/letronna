**89 tasks, 89 trials.** Mean reward 0.573; 51 passed every run.
The oracle failed 5 of these here (caffe-cifar-10, make-doom-for-mips, protein-assembly, qemu-alpine-ssh, qemu-startup); on the other 84, mean reward 0.595.
Errors: NonZeroAgentExitCodeError 3, VerifierTimeoutError 1.
Per task: 531K tokens with cache reads, 9.1 min; cost $0.00 in all.

| Task | Runs | Reward | Oracle | Error | Tokens | Minutes |
|---|---|---|---|---|---|---|
| adaptive-rejection-sampler | 1 | 0.00 |  |  | 387K | 6.7 |
| bn-fit-modify | 1 | 1.00 |  |  | 82K | 1.3 |
| break-filter-js-from-html | 1 | 1.00 |  |  | 124K | 4.0 |
| build-cython-ext | 1 | 1.00 |  |  | 2969K | 13.0 |
| build-pmars | 1 | 1.00 |  |  | 543K | 3.6 |
| build-pov-ray | 1 | 1.00 |  |  | 490K | 3.2 |
| caffe-cifar-10 | 1 | 0.00 | 0 |  | 2086K | 18.0 |
| cancel-async-tasks | 1 | 0.00 |  |  | 96K | 3.7 |
| chess-best-move | 1 | 1.00 |  |  | 123K | 2.8 |
| circuit-fibsqrt | 1 | 1.00 |  |  | 166K | 7.8 |
| cobol-modernization | 1 | 1.00 |  |  | 452K | 13.0 |
| code-from-image | 1 | 1.00 |  |  | 30K | 0.9 |
| compile-compcert | 1 | 0.00 |  |  | 1196K | 14.7 |
| configure-git-webserver | 1 | 0.00 |  |  | 383K | 3.8 |
| constraints-scheduling | 1 | 1.00 |  |  | 164K | 2.1 |
| count-dataset-tokens | 1 | 1.00 |  |  | 202K | 2.3 |
| crack-7z-hash | 1 | 0.00 |  |  | 1351K | 13.0 |
| custom-memory-heap-crash | 1 | 0.00 |  |  | 26K | 0.3 |
| db-wal-recovery | 1 | 0.00 |  |  | 573K | 10.2 |
| distribution-search | 1 | 1.00 |  |  | 170K | 2.5 |
| dna-assembly | 1 | 0.00 |  |  | 1909K | 28.0 |
| dna-insert | 1 | 1.00 | 1 |  | 779K | 14.1 |
| extract-elf | 1 | 0.00 |  |  | 289K | 5.2 |
| extract-moves-from-video | 1 | 0.00 | 1 |  | 1172K | 14.3 |
| feal-differential-cryptanalysis | 1 | 1.00 |  |  | 237K | 4.6 |
| feal-linear-cryptanalysis | 1 | 1.00 | 1 |  | 201K | 6.3 |
| filter-js-from-html | 1 | 0.00 |  |  | 1612K | 18.9 |
| financial-document-processor | 1 | 0.00 |  |  | 2386K | 18.0 |
| fix-code-vulnerability | 1 | 1.00 |  |  | 254K | 3.3 |
| fix-git | 1 | 0.00 |  |  | 416K | 4.2 |
| fix-ocaml-gc | 1 | 1.00 |  |  | 946K | 11.6 |
| gcode-to-text | 1 | 0.00 | 1 |  | 807K | 6.2 |
| git-leak-recovery | 1 | 1.00 |  |  | 152K | 2.3 |
| git-multibranch | 1 | 1.00 |  |  | 306K | 3.8 |
| gpt2-codegolf | 1 | 0.00 | 1 |  | 357K | 13.0 |
| headless-terminal | 1 | 1.00 |  |  | 444K | 9.3 |
| hf-model-inference | 1 | 1.00 |  |  | 301K | 5.8 |
| install-windows-3.11 | 1 | 0.00 | 1 |  | 373K | 5.1 |
| kv-store-grpc | 1 | 1.00 |  |  | 218K | 3.7 |
| large-scale-text-editing | 1 | 1.00 |  |  | 391K | 8.6 |
| largest-eigenval | 1 | 1.00 |  |  | 275K | 11.8 |
| llm-inference-batching-scheduler | 1 | 0.00 |  |  | 267K | 6.4 |
| log-summary-date-ranges | 1 | 1.00 |  |  | 201K | 3.2 |
| mailman | 1 | 0.00 |  |  | 506K | 5.2 |
| make-doom-for-mips | 1 | 0.00 | 0 |  | 1396K | 13.0 |
| make-mips-interpreter | 1 | 0.00 | 1 |  | 117K | 1.1 |
| mcmc-sampling-stan | 1 | 1.00 |  |  | 914K | 21.7 |
| merge-diff-arc-agi-task | 1 | 1.00 |  |  | 623K | 8.1 |
| model-extraction-relu-logits | 1 | 1.00 |  |  | 189K | 4.5 |
| modernize-scientific-stack | 1 | 1.00 |  |  | 240K | 3.7 |
| mteb-leaderboard | 1 | 0.00 |  |  | 1127K | 15.1 |
| mteb-retrieve | 1 | 0.00 |  |  | 170K | 9.6 |
| multi-source-data-merger | 1 | 1.00 |  |  | 172K | 3.5 |
| nginx-request-logging | 1 | 1.00 |  |  | 213K | 3.7 |
| openssl-selfsigned-cert | 1 | 1.00 |  |  | 204K | 2.6 |
| overfull-hbox | 1 | 1.00 | 1 |  | 615K | 7.2 |
| password-recovery | 1 | 1.00 |  |  | 773K | 8.8 |
| path-tracing | 1 | 1.00 |  |  | 2307K | 22.8 |
| path-tracing-reverse | 1 | 0.00 | 1 |  | 158K | 1.2 |
| polyglot-c-py | 1 | 1.00 | 1 |  | 220K | 7.3 |
| polyglot-rust-c | 1 | 1.00 | 1 |  | 100K | 4.5 |
| portfolio-optimization | 1 | 0.00 |  |  | 281K | 7.0 |
| protein-assembly | 1 | 0.00 | 0 |  | 1067K | 12.6 |
| prove-plus-comm | 1 | 1.00 |  |  | 217K | 4.4 |
| pypi-server | 1 | 1.00 |  |  | 399K | 6.5 |
| pytorch-model-cli | 1 | 1.00 |  |  | 309K | 4.4 |
| pytorch-model-recovery | 1 | 1.00 |  |  | 233K | 6.3 |
| qemu-alpine-ssh | 1 | 0.00 | 0 |  | 624K | 13.0 |
| qemu-startup | 1 | 1.00 | 0 |  | 854K | 11.2 |
| query-optimize | 1 | 0.00 | 1 |  | 165K | 5.9 |
| raman-fitting | 1 | 0.00 | 1 |  | 837K | 11.3 |
| regex-chess | 1 | 0.00 | 1 | NonZeroAgentExitCodeError | 7K | 11.9 |
| regex-log | 1 | 1.00 |  |  | 95K | 3.6 |
| reshard-c4-data | 1 | 1.00 | 1 |  | 723K | 15.6 |
| rstan-to-pystan | 1 | 1.00 |  |  | 991K | 26.5 |
| sam-cell-seg | 1 | 0.00 | 1 | NonZeroAgentExitCodeError | 82K | 10.8 |
| sanitize-git-repo | 1 | 1.00 |  |  | 796K | 9.9 |
| schemelike-metacircular-eval | 1 | 0.00 |  |  | 413K | 2.6 |
| sparql-university | 1 | 1.00 |  |  | 594K | 12.1 |
| sqlite-db-truncate | 1 | 1.00 |  |  | 93K | 2.0 |
| sqlite-with-gcov | 1 | 1.00 |  |  | 347K | 7.1 |
| torch-pipeline-parallelism | 1 | 0.00 |  | VerifierTimeoutError | 387K | 11.4 |
| torch-tensor-parallelism | 1 | 0.00 |  |  | 178K | 12.4 |
| train-fasttext | 1 | 0.00 | 1 |  | 373K | 58.1 |
| tune-mjcf | 1 | 1.00 | 1 |  | 412K | 13.0 |
| video-processing | 1 | 0.00 | 1 |  | 499K | 13.6 |
| vulnerable-secret | 1 | 1.00 |  |  | 117K | 1.8 |
| winning-avg-corewars | 1 | 0.00 | 1 | NonZeroAgentExitCodeError | 161K | 21.0 |
| write-compressor | 1 | 0.00 | 1 |  | 31K | 13.1 |
