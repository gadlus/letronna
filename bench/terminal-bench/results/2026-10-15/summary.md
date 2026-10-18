**89 tasks, 89 trials.** Mean reward 0.652; 58 passed every run.
The oracle failed 7 of these here (build-cython-ext, build-pmars, caffe-cifar-10, make-doom-for-mips, protein-assembly, qemu-alpine-ssh, qemu-startup); on the other 82, mean reward 0.683.
Errors: AgentTimeoutError 18, NonZeroAgentExitCodeError 8.
Per task: 1339K tokens with cache reads, 9.9 min; cost $0.00 in all.

| Task | Runs | Reward | Oracle | Error | Tokens | Minutes |
|---|---|---|---|---|---|---|
| adaptive-rejection-sampler | 1 | 0.00 | 1 | NonZeroAgentExitCodeError | 33K | 4.4 |
| bn-fit-modify | 1 | 1.00 | 1 |  | 498K | 4.1 |
| break-filter-js-from-html | 1 | 1.00 | 1 |  | 194K | 2.3 |
| build-cython-ext | 1 | 1.00 | 0 |  | 1209K | 5.3 |
| build-pmars | 1 | 1.00 | 0 |  | 315K | 1.8 |
| build-pov-ray | 1 | 1.00 | 1 |  | 2147K | 5.4 |
| caffe-cifar-10 | 1 | 0.00 | 0 | AgentTimeoutError | 266K | 20.0 |
| cancel-async-tasks | 1 | 1.00 | 1 |  | 791K | 5.4 |
| chess-best-move | 1 | 1.00 | 1 |  | 337K | 1.9 |
| circuit-fibsqrt | 1 | 0.00 | 1 | NonZeroAgentExitCodeError | 14K | 14.6 |
| cobol-modernization | 1 | 1.00 | 1 | AgentTimeoutError | 2594K | 15.0 |
| code-from-image | 1 | 1.00 | 1 |  | 19K | 0.2 |
| compile-compcert | 1 | 1.00 |  |  | 920K | 18.6 |
| configure-git-webserver | 1 | 0.00 | 1 |  | 301K | 2.6 |
| constraints-scheduling | 1 | 1.00 | 1 |  | 78K | 0.6 |
| count-dataset-tokens | 1 | 1.00 | 1 |  | 472K | 4.2 |
| crack-7z-hash | 1 | 1.00 | 1 |  | 175K | 2.9 |
| custom-memory-heap-crash | 1 | 1.00 |  |  | 1087K | 4.9 |
| db-wal-recovery | 1 | 1.00 |  |  | 585K | 2.4 |
| distribution-search | 1 | 1.00 |  |  | 210K | 3.3 |
| dna-assembly | 1 | 1.00 |  |  | 1810K | 11.7 |
| dna-insert | 1 | 0.00 | 1 |  | 780K | 5.0 |
| extract-elf | 1 | 1.00 |  |  | 884K | 4.3 |
| extract-moves-from-video | 1 | 0.00 | 1 | AgentTimeoutError | 881K | 30.0 |
| feal-differential-cryptanalysis | 1 | 1.00 |  |  | 517K | 5.6 |
| feal-linear-cryptanalysis | 1 | 0.00 | 1 | NonZeroAgentExitCodeError | 6K | 3.0 |
| filter-js-from-html | 1 | 0.00 |  |  | 949K | 4.7 |
| financial-document-processor | 1 | 1.00 |  |  | 424K | 2.4 |
| fix-code-vulnerability | 1 | 1.00 |  |  | 113K | 0.5 |
| fix-git | 1 | 1.00 |  |  | 115K | 0.9 |
| fix-ocaml-gc | 1 | 1.00 |  |  | 2383K | 12.0 |
| gcode-to-text | 1 | 0.00 | 1 | AgentTimeoutError | 2590K | 15.0 |
| git-leak-recovery | 1 | 1.00 |  |  | 65K | 0.6 |
| git-multibranch | 1 | 1.00 |  |  | 462K | 3.3 |
| gpt2-codegolf | 1 | 0.00 | 1 | AgentTimeoutError | 3526K | 15.0 |
| headless-terminal | 1 | 1.00 |  |  | 3037K | 12.9 |
| hf-model-inference | 1 | 1.00 |  |  | 81K | 1.4 |
| install-windows-3.11 | 1 | 0.00 | 1 |  | 3907K | 19.4 |
| kv-store-grpc | 1 | 1.00 |  |  | 99K | 0.9 |
| large-scale-text-editing | 1 | 1.00 |  |  | 404K | 7.3 |
| largest-eigenval | 1 | 1.00 |  | AgentTimeoutError | 3610K | 15.0 |
| llm-inference-batching-scheduler | 1 | 1.00 |  |  | 1600K | 8.1 |
| log-summary-date-ranges | 1 | 1.00 |  |  | 98K | 0.7 |
| mailman | 1 | 1.00 |  |  | 2861K | 13.3 |
| make-doom-for-mips | 1 | 0.00 | 0 | AgentTimeoutError | 2930K | 15.0 |
| make-mips-interpreter | 1 | 0.00 | 1 | AgentTimeoutError | 9725K | 30.0 |
| mcmc-sampling-stan | 1 | 1.00 |  |  | 535K | 11.5 |
| merge-diff-arc-agi-task | 1 | 1.00 |  |  | 191K | 1.6 |
| model-extraction-relu-logits | 1 | 1.00 |  | AgentTimeoutError | 760K | 15.0 |
| modernize-scientific-stack | 1 | 1.00 |  |  | 42K | 0.6 |
| mteb-leaderboard | 1 | 1.00 |  |  | 490K | 21.5 |
| mteb-retrieve | 1 | 0.00 |  |  | 119K | 1.9 |
| multi-source-data-merger | 1 | 1.00 |  |  | 89K | 1.4 |
| nginx-request-logging | 1 | 1.00 |  |  | 128K | 2.0 |
| openssl-selfsigned-cert | 1 | 1.00 |  |  | 106K | 1.4 |
| overfull-hbox | 1 | 0.00 | 1 | AgentTimeoutError | 288K | 12.5 |
| password-recovery | 1 | 1.00 |  |  | 192K | 2.3 |
| path-tracing | 1 | 1.00 |  |  | 881K | 5.4 |
| path-tracing-reverse | 1 | 0.00 | 1 | NonZeroAgentExitCodeError | 505K | 13.7 |
| polyglot-c-py | 1 | 0.00 | 1 |  | 372K | 5.6 |
| polyglot-rust-c | 1 | 0.00 | 1 | AgentTimeoutError | 237K | 15.0 |
| portfolio-optimization | 1 | 1.00 |  |  | 342K | 6.3 |
| protein-assembly | 1 | 0.00 | 0 |  | 452K | 9.8 |
| prove-plus-comm | 1 | 1.00 |  |  | 50K | 0.9 |
| pypi-server | 1 | 1.00 |  |  | 105K | 2.0 |
| pytorch-model-cli | 1 | 1.00 |  |  | 188K | 2.6 |
| pytorch-model-recovery | 1 | 1.00 |  |  | 1138K | 5.7 |
| qemu-alpine-ssh | 1 | 0.00 | 0 | AgentTimeoutError | 728K | 15.0 |
| qemu-startup | 1 | 0.00 | 0 | AgentTimeoutError | 996K | 15.0 |
| query-optimize | 1 | 0.00 | 1 |  | 96K | 4.3 |
| raman-fitting | 1 | 0.00 | 1 | AgentTimeoutError | 595K | 15.0 |
| regex-chess | 1 | 0.00 | 1 | NonZeroAgentExitCodeError | 6K | 4.8 |
| regex-log | 1 | 1.00 |  |  | 242K | 10.5 |
| reshard-c4-data | 1 | 0.00 | 1 |  | 4018K | 20.9 |
| rstan-to-pystan | 1 | 1.00 |  |  | 569K | 11.3 |
| sam-cell-seg | 1 | 0.00 | 1 | AgentTimeoutError | 38953K | 120.0 |
| sanitize-git-repo | 1 | 1.00 |  |  | 283K | 2.1 |
| schemelike-metacircular-eval | 1 | 1.00 |  | AgentTimeoutError | 4658K | 40.0 |
| sparql-university | 1 | 1.00 |  |  | 135K | 1.5 |
| sqlite-db-truncate | 1 | 1.00 |  |  | 277K | 3.1 |
| sqlite-with-gcov | 1 | 1.00 |  |  | 254K | 3.3 |
| torch-pipeline-parallelism | 1 | 1.00 |  |  | 1188K | 14.5 |
| torch-tensor-parallelism | 1 | 1.00 |  |  | 436K | 7.9 |
| train-fasttext | 1 | 0.00 | 1 | AgentTimeoutError | 311K | 60.0 |
| tune-mjcf | 1 | 0.00 | 1 | AgentTimeoutError | 1278K | 15.0 |
| video-processing | 1 | 0.00 | 1 | NonZeroAgentExitCodeError | 232K | 3.3 |
| vulnerable-secret | 1 | 1.00 |  |  | 115K | 0.9 |
| winning-avg-corewars | 1 | 0.00 | 1 | NonZeroAgentExitCodeError | 449K | 9.9 |
| write-compressor | 1 | 0.00 | 1 | NonZeroAgentExitCodeError | 6K | 4.3 |
