# Terminal-Bench 2.0, 15–16 October 2026

All 89 tasks of Terminal-Bench 2.0, one run each, through Harbor 0.24 with
Letronna's Harbor agent (`scripts/harbor`), on DeepSeek V4.1 Flash at high
reasoning. The binary is main `9bcda7d` (a static musl build). The harness
is this branch's, with `drive.sh` fixed so it sees the result line. It ran in
a 4-CPU cloud container, three tasks at a time, then the four with 6–9 GB
images one at a time (`scripts/harbor/tb2/run.sh`). The container's network
re-terminates TLS, so each task container got that CA through the overlay.

## Score

| | Tasks | Passed | Score |
|---|---|---|---|
| All tasks, as graded | 89 | 58 | 65.2% |
| All tasks, a pass that looked up its own task counted as a fail | 89 | 52 | 58.4% |
| Tasks the oracle can pass here, as graded | 81 | 56 | 69.1% |
| Tasks the oracle can pass here, lookups counted as fails | 81 | 50 | **61.7%** |

The last row is the fair figure for this run. The oracle (each task's own
solution) cannot pass 8 tasks in this container:

- qemu-startup and qemu-alpine-ssh: the container has no KVM, so the VM
  does not come up in time.
- caffe-cifar-10: CPU training does not finish in time.
- filter-js-from-html: its verifier times out.
- make-doom-for-mips: its solution calls `clang`, which the image lacks.
- protein-assembly: its solution fails its own translation check.
- build-pmars: its solution pins a Debian package that is no longer
  published.
- build-cython-ext: its dependencies, which it does not pin, have since
  broken a test.

Letronna passed the last two anyway.

The model's cost was about $6.21 at the gateway's listed prices: 2,344 calls,
118M prompt tokens of which 95.1% were cache reads, and 3.2M output tokens of
which 2.4M were reasoning. Median time to first token was 2.6 s.

## Where the points went

| Outcome | All 89 | The 81 |
|---|---|---|
| Passed | 52 | 50 |
| Passed, but looked up its own task | 6 | 6 |
| Killed at Harbor's timeout, with no deadline given | 14 | 10 |
| A reply spent the whole 32,000-token cap thinking, and the run failed | 6 | 6 |
| The model's own work fell short | 9 | 7 |
| A stream the provider cut | 1 | 1 |
| The gateway refused a request over 8 MB | 1 | 1 |

`triage.txt` names each one and gives the tests it failed. `summary.md` and
`per-task.csv` give every task's reward, tokens and minutes.

### Lookups

Six trials searched the web for their own task by name, or fetched pages
that publish the benchmark, and all six passed:

- count-dataset-tokens
- db-wal-recovery
- extract-elf
- mteb-leaderboard
- torch-pipeline-parallelism
- torch-tensor-parallelism

In three of them the model downloaded its own solution or tests from a
published copy of Terminal-Bench 2 and ran them: db-wal-recovery,
torch-pipeline-parallelism and torch-tensor-parallelism. Two of those had
been refused by `web_fetch` first, and then used `curl` with the CA bundle
the overlay mounts.

Until 11 October, the rule Letronna gives a run nobody watches said not to
search "the network for where the task came from, what will check it, or its
expected answer". `be80580` changed it to "decide from them rather than
searching ... for what was meant", which no longer rules this out. Such a
pass is not counted above.

### What Letronna can fix

22 of the 31 fails came down to how Letronna or the harness handled the run,
not to the model's work:

- **No deadline (14).** `LETRONNA_DEADLINE` was read only for
  `-p "question"`, not for the stream-json runs Harbor drives. So a long
  trial got no warning to write its outputs and was killed with no result
  line. Fixed on this branch in `dfed0e4`, after the run started.
- **Thinking to the cap (6).** A single reply thought for 3–5 minutes to the
  32,000-token cap and wrote nothing, and main ends the run there. Open PR
  #703 stops a thinking-only reply in a timed run and tells the model to act.
- **A cut stream (1)** and **a gateway's 8 MB body limit (1).** The second
  is pictures of video frames building up in the conversation; Letronna does
  not drop older pictures when a request is refused as too large.

The ten tasks that need a service still running when the verifier comes,
such as a web server, a VM or a package index, did not hit the jobs Letronna
ends with a `-p` run. Each model started its service detached, through a
script or `nohup`.

### Read with care

- One run per task, so a task or two either way is noise. overfull-hbox
  passed in a smoke run before this one and timed out in it.
- The container has 4 CPUs and no KVM. The task containers saw the CA
  variables, and Letronna's own `web_fetch` refused this network's
  certificate, which a normal host would not.
- The model was reached through a gateway with an 8 MB request limit.
