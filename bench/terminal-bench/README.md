# Terminal-Bench 2.0

Letronna's runs of [Terminal-Bench 2.0](https://www.tbench.ai/) (89 tasks)
through Harbor and Letronna's Harbor agent (`scripts/harbor`), one trial per
task, on one 4-CPU host without KVM. The fair figure leaves out the tasks the
reference solution cannot pass in this container and counts a pass that looked
up its own task as a fail.

| Results | Build | Model | Fair figure |
|---|---|---|---|
| [15–16 October](results/2026-10-15/README.md) | run 1 | DeepSeek V4.1 Flash | 50/81 (61.7%) |
| [16 October](results/2026-10-16/README.md) | run 2 | DeepSeek V4.1 Flash | 57/81 (70.4%) |
| [16–18 October](results/2026-10-17/README.md) | improved | DeepSeek V4.1 Flash | 57/78 (73.1%) |
| | improved | Qwen 3.8 Flash | 51/77 (66.2%) |
| | improved | Muse Spark 1.3 Contributor | 48/78 (61.5%) |
| | improved | GLM 5.3 (partial, 43 tasks) | 31/39 (79.5%) |
| | improved | DeepSeek V4 Pro (partial, 22 tasks) | 13/19 (68.4%) |
| | improved | Opus 5.5 (partial, 26 tasks) | 14/22 (63.6%) |

The partial runs each cover a different subset of the tasks; the 16–18
October results compare them with the other models on the same tasks.
