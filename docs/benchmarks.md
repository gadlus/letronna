# Benchmarks

The full write-up, with a task explorer and the raw data, is at [letronna.ai/benchmark](https://letronna.ai/benchmark).

## Speed

These are the figures in `bench/baseline.json`. They were measured with a release build on a Mac mini M4 (16 GB, macOS 26.2), and each is the median of 7 runs. Lower is better.

| Metric | Result |
|---|---|
| Cold start to a ready prompt (500 sessions on disk) | 7.65 ms |
| Memory at idle (RSS) | 9.52 MB |
| List 500 sessions | 1.47 ms |
| List 5,000 sessions (~90 MB on disk) | 16.0 ms |
| Full-text search, 5,000 sessions | 102 ms |
| Open a 10,000-message session | 11.8 ms |
| Fit a 200k-token window over it | 0.52 ms |
| Parse a 10,000-event stream | 13.2 ms |

## Harness-Bench

[Harness-Bench](https://github.com/Qihoo360/harness-bench) has 106 agent tasks. Each is scored 0–100 by an outcome oracle, a security gate and an LLM judge. The run used Letronna main at `0bc18970`, with one run per task, on 6–7 October 2026.

| Model | Letronna | Paper's mean for that model |
|---|---|---|
| DeepSeek V4 Flash | 84.6 | 57.5 |
| Claude Sonnet 4.6 | 81.2 | 65.3 |

For reference, the paper's best configurations on GPT-5.4 are NanoBot (81.3), Codex (80.4), Moltis (74.4) and Hermes (74.0).
