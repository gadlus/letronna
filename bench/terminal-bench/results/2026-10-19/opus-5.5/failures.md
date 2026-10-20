# Opus 5.5: why each failed trial lost its points

21 of the 89 tasks with a valid trial did not pass (a pass after looking up the task counts as a fail).

| Outcome | Tasks |
|---|---|
| The model's own work fell short | 6 |
| Stopped at the deadline with the work unfinished | 1 |
| Letronna's handling cost the task | 1 |
| Failed, and the oracle cannot pass it here either | 2 |
| The verifier timed out | 1 |
| The provider's content filter stopped the reply | 9 |
| Other | 1 |

## The model's own work fell short (6)

- **compile-compcert** (done (22 rounds, 970 s)): The model ended its turn after 970 s of a 38-minute budget, before any deadline warning. At that point the CompCert build was about half done (77 of 144 proof files) and /tmp/CompCert/ccomp did not exist yet. With about 22 minutes left, it said it "couldn't finish the build in the time this run had". All three tests failed because ccomp did not exist.
- **configure-git-webserver** (done (8 rounds, 94 s)): test_hello_html_exists failed with "Web server returned HTTP 404". The verifier clones and pushes as git@localhost with the password "password", but the model set up only a key-login account named user, so the verifier's push never arrived. The model's own push test as user had worked.
- **raman-fitting** (done (7 rounds, 74 s)): test_2D_Peak failed on the offset: 1433.5 where 1239.09 was expected. x0, gamma and amplitude were within tolerance. The model fitted one Lorentzian on a +/-250 cm-1 window, although its own window check moved the 2D offset from 814 to 1503. It ended after 74 s of a 13-minute budget.
- **sam-cell-seg** (done (18 rounds, 2062 s)): test_coords_are_flat_lists failed, and the other 8 tests passed. coords_x and coords_y were written as bare comma-joined strings like "72,72,73,..." (the input's format). The test's ast.literal_eval reads those as a tuple, not a list.
- **video-processing** (done (13 rounds, 725 s)): test_jump_analyzer_test_video failed: on the hidden test video the script gave takeoff frame 186, outside the expected 219-223. The example video passed (frames 53 and 62). The model ended after 725 s of a 58-minute budget.
- **winning-avg-corewars** (done (12 rounds, 658 s)): test_warrior_performance failed: the warrior won 73 of 100 against stone.red (needs 75) and 15 against snake.red (needs 33). It passed vampire, paper and g2-clear. The model knew it missed 2 of the 5 thresholds and ended after 658 s of a 58-minute budget.

## Stopped at the deadline with the work unfinished (1)

- **rstan-to-pystan** (done (24 rounds, 1392 s)): Installing PyStan 3.10.0 took about 8 minutes, and a C compiler another half minute. Sampling was then at 45% (3602 of 8000 iterations) when Letronna's 335 s warning came. The model ended its turn at 1392 s, not stopped by Letronna, because the sampling could not finish in the time left. No alpha_est.csv, sigma_est.csv, rho_est.csv or beta_est.csv was written, so test_output_files_exist and the four estimation-accuracy tests failed. Only test_r_rstan_not_installed passed.

## Letronna's handling cost the task (1)

- **mteb-leaderboard** (done (23 rounds, 349 s)): test_data_matches failed because result.txt held Qwen/Qwen3-Embedding-8B, a placeholder the model wrote with a shell echo in round 1. It later computed the expected GritLM/GritLM-7B from the MTEB results repo. After a web search, though, Letronna refused both attempts to overwrite result.txt, first by shell and then by write_file, as changing "a file the session did not make", although the session had made it. Each refusal told the model not to look for another way, so it ended at 349 s with the wrong answer saved. 4 calls were refused in all.

## Failed, and the oracle cannot pass it here either (2)

- **caffe-cifar-10** (done (24 rounds, 833 s)): The oracle cannot pass this task in this container. Opus 5.5 built Caffe 1.0 for CPU, and test_caffe_version_and_source passed. compute_image_mean needs OpenCV, so the model removed mean subtraction and started training late. When it ended at 833 s, training was at iteration 300 of 500 with the loss still at 2.30. test_cifar10_model_exists (no cifar10_quick_iter_500.caffemodel), test_training_completed_500_iterations and test_model_accuracy_verification failed.
- **qemu-alpine-ssh** (done (8 rounds, 631 s)): The oracle cannot pass this task in this container. The model started QEMU with port 2222 forwarded, but in about 10 minutes its expect script never got a login prompt on the serial console. It ended at Letronna's 155 s warning with SSH not set up, so test_sshpass failed (ssh exit status 255).

## The verifier timed out (1)

- **query-optimize** (done (6 rounds, 426 s)): The verifier timed out after 900 s (VerifierTimeoutError, reward None) before any of its 6 tests reported. Its first test runs the original my-sql-query.sql on the unindexed database, which the model had seen still running after more than 6 minutes. The model's sol.sql ran in 1.6 s and gave the same 500 rows as the original on an indexed scratch copy.

## The provider's content filter stopped the reply (9)

- **break-filter-js-from-html** (failed (0 rounds, 1 s)): The run ended within 1 s, on its first reply (0 rounds), with "The provider's content filter stopped the reply." /app/out.html was never written, so test_out_html_bypasses_filter failed.
- **crack-7z-hash** (failed (0 rounds, 1 s)): The run ended within 1 s, on its first reply (0 rounds), with "The provider's content filter stopped the reply." /app/solution.txt was never written, so test_solution_file and test_solution_content failed.
- **dna-assembly** (failed (0 rounds, 1 s)): The run ended within 1 s, on its first reply (0 rounds), with "The provider's content filter stopped the reply." /app/primers.fasta was never written, so test_primers failed.
- **dna-insert** (failed (1 round, 10 s)): The run ended after 10 s, on its second reply, with "The provider's content filter stopped the reply." Its one command had listed /app and printed sequences.fasta. /app/primers.fasta was never written, so test_primers failed.
- **feal-differential-cryptanalysis** (failed (1 round, 11 s)): The run ended after 11 s, on its second reply, with "The provider's content filter stopped the reply." Its one call had read /app/feal.py. No attack.py was written, so test_feal_differential_cryptanalysis_attack failed with "No module named 'attack'".
- **filter-js-from-html** (failed (0 rounds, 136 s)): The run ended on its first reply (0 rounds), after 136 s, with "The provider's content filter stopped the reply." No /app/filter.py was written, so test_filter_blocks_xss and test_clean_html_unchanged failed. The oracle cannot pass this task in this container either.
- **password-recovery** (failed (0 rounds, 2 s)): The run ended after 2 s, on its first reply (0 rounds), with "The provider's content filter stopped the reply." /app/recovered_passwords.txt was never written, so test_recovery_file_exists and test_password_match failed.
- **protein-assembly** (failed (0 rounds, 1 s)): The run ended within 1 s, on its first reply (0 rounds), with "The provider's content filter stopped the reply." /app/gblock.txt was never written, so test_gblock failed. The oracle cannot pass this task in this container either.
- **vulnerable-secret** (failed (0 rounds, 1 s)): The run ended within 1 s, on its first reply (0 rounds), with "The provider's content filter stopped the reply." /app/results.txt was never written, so test_results_file_exists, test_results_file_contains_secret_flag and test_results_file_format failed.

## Other (1)

- **extract-moves-from-video** (done (26 rounds, 333 s)): YouTube blocked the video. yt-dlp (with deno) got the video's details, but every download returned HTTP 403 on the web, embedded, TV, android_vr, iOS, mweb and Safari clients, several of them with "Sign in to confirm you're not a bot". The Invidious mirrors failed too. With no video, the model did not write /app/solution.txt, so test_solution_file_exists and test_solution_content_similarity failed.
