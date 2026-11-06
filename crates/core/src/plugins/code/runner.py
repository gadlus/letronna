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
    windows_handle(STD_OUTPUT_HANDLE, 1)
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
    run from a tool, never started."""
    null = os.open(os.devnull, os.O_RDONLY)
    os.dup2(null, 0)
    os.close(null)
    windows_handle(STD_INPUT_HANDLE, 0)


STD_INPUT_HANDLE, STD_OUTPUT_HANDLE = -10, -11


def windows_handle(which, fd):
    """On Windows, makes the standard handle `which` the one under `fd`:
    Python hands a program Windows' own standard handles, not fds 0 and 1,
    so without this a program a plugin starts would still be given the
    messages' pipes. Says so in the log when Windows refuses."""
    if os.name != "nt":
        return
    import ctypes
    import msvcrt

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.SetStdHandle.argtypes = (ctypes.c_uint32, ctypes.c_void_p)
    kernel32.SetStdHandle.restype = ctypes.c_int
    if not kernel32.SetStdHandle(which & 0xFFFFFFFF, msvcrt.get_osfhandle(fd)):
        sys.stderr.write(
            "letronna: Windows' standard handle %d could not be set (error %d); "
            "a program this plugin starts may be handed the messages' pipe\n"
            % (which, ctypes.get_last_error())
        )


if __name__ == "__main__":
    main()
