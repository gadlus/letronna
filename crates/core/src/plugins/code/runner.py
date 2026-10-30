"""Runs one Letronna plugin written in Python.

Letronna starts this with the plugin's folder and its main file, and sends
it one JSON object a line on stdin, its hello first; it answers with lines
of JSON on stdout. Only this file writes to the real stdout: print() and
anything else that writes there, C extensions included, goes to stderr,
which is the plugin's log, so nothing a plugin prints can break the
messages. It needs nothing but Python's own library.
"""

import json
import os
import runpy
import sys
import traceback


def main():
    folder, main_file = sys.argv[1], sys.argv[2]
    messages = os.fdopen(os.dup(1), "w", encoding="utf-8", newline="\n", buffering=1)
    os.dup2(2, 1)
    sys.stdout = sys.stderr
    here = os.path.dirname(os.path.abspath(__file__))
    sys.path[:0] = [here, folder]
    import letronna

    hello = json.loads(sys.stdin.buffer.readline().decode("utf-8"))
    letronna._start(messages, hello)
    try:
        runpy.run_path(os.path.join(folder, main_file), run_name="__letronna__")
    except BaseException:
        letronna._send({"do": "error", "text": "its main file failed: " + traceback.format_exc()})
        sys.stderr.flush()
        os._exit(1)
    letronna._serve(sys.stdin.buffer)
    # Letronna closed stdin, so it is gone or wants this gone; threads still
    # working have nobody to answer.
    sys.stderr.flush()
    os._exit(0)


if __name__ == "__main__":
    main()
