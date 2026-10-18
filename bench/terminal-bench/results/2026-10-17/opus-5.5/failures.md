# Opus 5.5: why each failed trial lost its points

A partial run: 33 trials started on 16 October before it was stopped. The 26
before the gateway's rate limit are scored; the 7 that ran into it, or were
cancelled when the run was stopped, are not (extract-moves-from-video,
financial-document-processor, fix-code-vulnerability, fix-git, fix-ocaml-gc,
gcode-to-text and git-leak-recovery).

10 of the 26 scored trials did not pass.

| Outcome | Tasks |
|---|---|
| The provider's content filter stopped the reply | 7 |
| The model's own work fell short | 2 |
| Failed, and the oracle cannot pass it here either | 1 |

## The provider's content filter stopped the reply (7)

Each run ended within seconds, on its first or second reply, with "The
provider's content filter stopped the reply.": break-filter-js-from-html,
crack-7z-hash, dna-assembly, dna-insert, feal-differential-cryptanalysis,
feal-linear-cryptanalysis and filter-js-from-html. These count as fails.
Shortly after, the gateway began turning away every call on the key with a
rate limit (from 19:14:49 UTC), and the run was stopped.

## The model's own work fell short (2)

- **adaptive-rejection-sampler**: test_can_generate_standard_distribution_samples
  failed, the signature problem every model met (the test passes the domain as
  the second argument).
- **configure-git-webserver**: the page the verifier fetches was not served
  (test_hello_html_exists).

## Failed, and the oracle cannot pass it here either (1)

- **caffe-cifar-10**: no trained model at the deadline; all five tests failed.

`triage.txt` has each trial's result line and failed tests.
