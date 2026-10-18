# Terminal-Bench 2.0, 16 October 2026

The second run of all 89 tasks, one trial each, on the same model, host and
harness as [the first](../2026-10-15/README.md): DeepSeek V4.1 Flash at high
reasoning, through Harbor 0.24 with Letronna's Harbor agent
(`scripts/harbor`), three tasks at a time and then the four with 6–9 GB
images one at a time. Only Letronna changed. The binary is `7f467cb`: the
first run's build plus

- `dfed0e4`: a stream-json run keeps to `LETRONNA_DEADLINE`, so a trial is
  warned at 80% of its time and stops with its result line before Harbor's
  timeout, rather than being killed with nothing written;
- open PR #703: a timed run whose reply thinks to the cap without writing
  anything is told to act, rather than ending;
- the model's own requests timed against the deadline as well.

The harness still answered approvals over stdio, as in the first run. That
makes a run count as watched, so the rules Letronna gives a run nobody
watches were not sent, the rule against looking up the task included.
`aefed3b` fixes that for the next run.

## Score

| | Tasks | Run 1 | Run 2 |
|---|---|---|---|
| All tasks, as graded | 89 | 58 (65.2%) | 67 (75.3%) |
| All tasks, a pass that looked up its own task counted as a fail | 89 | 52 (58.4%) | 62 (69.7%) |
| Tasks the oracle can pass here, as graded | 81 | 56 (69.1%) | 62 (76.5%) |
| Tasks the oracle can pass here, lookups counted as fails | 81 | 50 (61.7%) | **57 (70.4%)** |

The last row is the fair figure, on the same 81 tasks as before: the oracle
cannot pass qemu-startup, qemu-alpine-ssh, caffe-cifar-10,
filter-js-from-html, make-doom-for-mips, protein-assembly, build-pmars or
build-cython-ext in this container. This time Letronna passed five of those
eight: both qemu tasks, protein-assembly and the two build tasks.

Harbor errors fell from 26 to 2. In run 1, 18 trials were killed at the
timeout and 8 ended with Letronna failing. In run 2, no trial was killed.
One verifier timed out (filter-js-from-html, as with the oracle), and one
run failed (merge-diff-arc-agi-task, below).

The model's cost was about $6.04 at the gateway's listed prices: 2,208
calls, 85.5M prompt tokens of which 94.6% were cache reads, and 3.5M output
tokens of which 2.8M were reasoning. That is 28% fewer prompt tokens than
run 1 for more passes. Median time to first token was 2.7 s.

## What changed, task by task

Fourteen tasks passed that failed in run 1, and five failed that passed.

Gained:

- **No longer killed at the timeout (7):** make-mips-interpreter,
  overfull-hbox, polyglot-rust-c, qemu-alpine-ssh, qemu-startup,
  raman-fitting and tune-mjcf. raman-fitting looked up its task, so it does
  not count in the fair figure.
- **No longer ended by a reply that only thought (2):**
  feal-linear-cryptanalysis and path-tracing-reverse, which run 1 lost to a
  reply that thought to the 32,000-token cap with nothing written. In run 2,
  feal-linear-cryptanalysis hit the cap again, was told to act, and went on
  to pass; path-tracing-reverse did not hit it.
- **Did not reach the gateway's 8 MB request limit (1):** video-processing.
- **The model's work came out differently (4):** configure-git-webserver,
  protein-assembly, reshard-c4-data and mteb-retrieve. mteb-retrieve looked
  up its task.

Lost:

- **merge-diff-arc-agi-task:** after 204 s, the gateway answered 200, sent
  keep-alives for two minutes, then sent `every lender we tried refused this
  request (HTTP 502)` as an event in the stream. Letronna found no kind in
  that error, so it was not retried and the run ended. Fixed for the next
  run: the status the words name is read, and an error event that names
  nothing counts as the provider's trouble and is retried.
- **pypi-server:** the model started the package index as a Letronna
  background job, checked that it served, and ended. A `-p` run stops its
  jobs when it ends, so by the time the verifier came there was nothing on
  port 8080. In run 1 the model had started the server with `nohup`. Fixed
  for the next run: in a run that ends with its turn, starting a background
  job says the job ends with the run and how to leave a service running.
- **extract-elf:** it passed in run 1 only after looking up its own task,
  so it was not counted in run 1's fair figure.
- **pytorch-model-cli and sanitize-git-repo:** the model's own work fell
  short. The first tool's predictions did not match on the checker's images.
  The second rewrote the repository's whole history to remove the secrets,
  so a commit the checker reads no longer existed.

## Where the points went

| Outcome | All 89 | The 81 |
|---|---|---|
| Passed | 62 | 57 |
| Passed, but looked up its own task | 5 | 5 |
| Stopped at the deadline with the work unfinished | 6 | 5 |
| The model's own work fell short | 12 | 12 |
| A stream error that was not retried | 1 | 1 |
| A service it started as a job was stopped when the run ended | 1 | 1 |
| Failed, and the oracle cannot pass it here either | 2 | – |

The sixth deadline stop is caffe-cifar-10, which the oracle cannot pass
here either. `triage.txt` names each failure and gives the tests it failed. `summary.md` and `per-task.csv` give every task's
reward, tokens and minutes.

### Lookups

Five trials looked up their own task, against six in run 1, and all five
passed: count-dataset-tokens, mteb-leaderboard, mteb-retrieve, raman-fitting
and torch-pipeline-parallelism. raman-fitting fetched the task's published
tests. torch-pipeline-parallelism and mteb-leaderboard tried to download
the task from the benchmark's repositories with `curl`. The rule against
this was not sent (see above), and is sent from the next run on.

### What is left

The five deadline stops among the 81 (adaptive-rejection-sampler,
gpt2-codegolf, regex-chess, train-fasttext and write-compressor) are hard
tasks the model did not finish in time. Two of them, regex-chess and
write-compressor, ran only 3 and 5 rounds: nearly all their time went to
replies that thought at length. Most of the 12 short-falls come down to the
model, not the run:
a misread digit in gcode-to-text, a reverse-complement helper that did
nothing on lowercase DNA in dna-insert, an optimisation that did not beat
the reference in query-optimize. The next build adds general changes aimed
at the parts Letronna can help with: what it tells the model about CPUs,
outputs and helpers, recovery from oversized requests and cut replies, and
end-of-run checks on files a command wrote.

### Read with care

- One run per task, so a task or two either way is noise. Run 2 gained 14
  and lost 5, and some of those 19 would flip again on a rerun.
- The container has 4 CPUs and no KVM. The task containers saw the CA
  variables, and Letronna's own `web_fetch` refused this network's
  certificate, which a normal host would not.
- The model was reached through a gateway with an 8 MB request limit.
