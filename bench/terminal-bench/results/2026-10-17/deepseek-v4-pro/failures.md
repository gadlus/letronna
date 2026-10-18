# DeepSeek V4 Pro: why each failed trial lost its points

7 of the 22 tasks with a valid trial did not pass (a pass after looking up the task counts as a fail).

| Outcome | Tasks |
|---|---|
| The model's own work fell short | 3 |
| Stopped at the deadline with the work unfinished | 1 |
| Failed, and the oracle cannot pass it here either | 1 |
| Looked up its own task | 2 |

## The model's own work fell short (3)

- **cancel-async-tasks** (done after 12 rounds, 431 s): test_tasks_cancel_above_max_concurrent failed: 2 'Task started.' lines and 0 'Cleaned up.' lines. run.py wraps gather() and, in its except block, cancels the workers again. With more tasks than slots, the queued task's cancellation ends the outer gather at once, and the second cancel interrupts the running tasks' async finally cleanup. The model only tested SIGINT with n_tasks == max_concurrent and a synchronous finally.
- **chess-best-move** (done after 31 rounds, 771 s): test_move_correct failed: it expects e2e4 and g2g4 (both mate in one), and move.txt held h1h3. The model rebuilt the board from ASCII renderings of the PNG into a wrong FEN in which White was down about 8 pawns, ran Stockfish on that FEN and wrote its top move. Its own -811 cp score was a sign of the misread, which it did not follow up; then four replies hit the thinking cap.
- **db-wal-recovery** (deadline after 20 rounds, 779 s): In round 4 the model opened /app/main.db with Python's sqlite3 without first backing up the encrypted main.db-wal. SQLite treated the invalid WAL as empty and deleted it when the connection closed. The model spent the rest of the run trying to undelete it (debugfs, mknod, find) and hit the thinking cap 4 times, so /app/recovered.json was never written and all 7 tests failed (test_recovered_json_exists, test_wal_was_decrypted, etc.).

## Stopped at the deadline with the work unfinished (1)

- **adaptive-rejection-sampler** (deadline after 5 rounds, 779 s): R was installed by 33 s. After that, all six replies streamed only reasoning_content (the meter shows the streams cut at 224, 157, 110, 77, 60 and 60 s). Letronna's thinking cap stopped and discarded each reply, so /app/ars.R was never written and all 9 tests failed (test_ars_function_exists, test_sample_files_generated, etc.).

## Failed, and the oracle cannot pass it here either (1)

- **caffe-cifar-10** (deadline after 50 rounds, 1079 s): Several things used up the time: the slow official CIFAR download (~64 KB/s), two refused rm/mv mirror commands and several rebuilds. Training only started at ~1012 s and had reached iteration 400 when the deadline hit, so test_training_completed_500_iterations and test_cifar10_model_exists failed. test_caffe_version_and_source and test_model_accuracy_verification failed because the model's Makefile.config left out BUILD_DIR, so the binary was built at ._release/tools/caffe.bin and not at .build_release/tools/caffe.bin.

## Looked up its own task (2)

- **count-dataset-tokens** (passed): Passed, but only after fetching its own task from the benchmark's published copy, so it counts as a fail.
- **extract-moves-from-video** (passed): Passed, but only after fetching its own task from the benchmark's published copy, so it counts as a fail.
