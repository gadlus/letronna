"""Runs one Letronna plugin written in Python.

Letronna starts this with the plugin's folder and its main file, and sends
it one JSON object a line on stdin, its hello first; it answers with lines
of JSON on stdout. Only this file writes to the real stdout: print() and
anything else that writes there, C extensions included, goes to stderr,
which is the plugin's log, so nothing a plugin prints can break the
messages. Only this file reads the real stdin, too: a program a plugin
starts reads nothing there. It needs nothing but Python's own library.
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
    incoming = os.fdopen(os.dup(0), "rb")
    read_nothing()
    here = os.path.dirname(os.path.abspath(__file__))
    sys.path[:0] = [here, folder]
    import letronna

    hello = json.loads(incoming.readline().decode("utf-8"))
    letronna._start(messages, hello)
    try:
        runpy.run_path(os.path.join(folder, main_file), run_name="__letronna__")
    except BaseException:
        letronna._send({"do": "error", "text": "its main file failed: " + traceback.format_exc()})
        sys.stderr.flush()
        os._exit(1)
    letronna._serve(incoming)
    # Letronna closed stdin, so it is gone or wants this gone; threads still
    # working have nobody to answer.
    sys.stderr.flush()
    os._exit(0)


def read_nothing():
    """Puts the null device where a program a plugin starts looks for its
    input, so it never meets the messages. It would read them away, and on
    Windows it would hang: a program that looks at a pipe another thread is
    reading waits for that read to end, which is the next message, so git,
    run from a tool, never started. Python hands a program Windows' own
    standard input handle rather than fd 0, so that is set as well."""
    null = os.open(os.devnull, os.O_RDONLY)
    os.dup2(null, 0)
    os.close(null)
    if os.name == "nt":
        import ctypes
        import msvcrt

        kernel32 = ctypes.WinDLL("kernel32")
        kernel32.SetStdHandle.argtypes = (ctypes.c_uint32, ctypes.c_void_p)
        STD_INPUT_HANDLE = -10 & 0xFFFFFFFF
        kernel32.SetStdHandle(STD_INPUT_HANDLE, msvcrt.get_osfhandle(0))


if __name__ == "__main__":
    main()
