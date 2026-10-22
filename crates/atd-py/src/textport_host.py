"""Atidarla textport: the console behind the Textport panel (docs/textport.md).

atd-py loads this module once per process. Each textport worker thread makes
a `Console` with its own namespace and runs what the user types through
`Console.push`, which answers whether more lines are needed.

Output is routed per thread: while a textport thread runs, `sys.stdout`,
`sys.stderr` and `sys.displayhook` send what it writes to that textport's
bridge (the Rust side); other threads (Python operators, expressions) write
where they always did.

The console API (`atidarla.nodes()`, `par`, `set_par`, `frame`, `time`, ...)
reads a snapshot of the project that the app publishes, and sends edits back
to the app, which applies them on its own thread through the undo history.
It works only on a textport thread.
"""

import ast
import builtins
import codeop
import importlib.machinery
import json
import os
import sys
import threading
import traceback

FILENAME = "<textport>"

# Per thread: `bridge` (the Rust side), `blocked` (an `ops/` folder that must
# not be imported from), `console` (the running Console), `snap` (cache).
_local = threading.local()


def _bridge():
    b = getattr(_local, "bridge", None)
    if b is None:
        raise RuntimeError("this function works only in the textport")
    return b


class _Stream:
    """`sys.stdout` or `sys.stderr`: a textport thread's writes go to its
    console, every other thread's to the stream that was there before."""

    _atd_textport = True

    def __init__(self, name, original):
        self._name = name
        self._original = original

    def write(self, text):
        b = getattr(_local, "bridge", None)
        if b is not None:
            if not isinstance(text, str):
                raise TypeError(f"write() argument must be str, not {type(text).__name__}")
            b.write(self._name, text)
            return len(text)
        if self._original is not None:
            return self._original.write(text)
        return len(text)

    def flush(self):
        if getattr(_local, "bridge", None) is None and self._original is not None:
            self._original.flush()

    def isatty(self):
        return False

    def writable(self):
        return True

    @property
    def encoding(self):
        return getattr(self._original, "encoding", "utf-8")

    @property
    def errors(self):
        return getattr(self._original, "errors", "strict")


def _displayhook(value):
    b = getattr(_local, "bridge", None)
    if b is None:
        return _displayhook.original(value)
    if value is None:
        return
    console = getattr(_local, "console", None)
    if console is not None:
        console.ns["_"] = None
    b.write("result", repr(value) + "\n")
    if console is not None:
        console.ns["_"] = value


def _inside(path, folder):
    path = os.path.normcase(os.path.realpath(path))
    folder = os.path.normcase(os.path.realpath(folder))
    return path == folder or path.startswith(folder.rstrip(os.sep) + os.sep)


class _OpsGuard:
    """Refuses imports from the open project's `ops/` folder on a textport
    thread while the project's Python operators are not allowed."""

    @classmethod
    def find_spec(cls, name, path=None, target=None):
        blocked = getattr(_local, "blocked", None)
        if not blocked:
            return None
        spec = importlib.machinery.PathFinder.find_spec(name, path)
        if spec is None:
            return None
        places = [spec.origin] + list(spec.submodule_search_locations or [])
        if any(p and _inside(p, blocked) for p in places):
            raise ImportError(
                f"`{name}` is in this project's ops/ folder, and the project's Python operators "
                "are not allowed to run (File > Allow Python Operators...)",
                name=name,
            )
        return None


def _install_routing():
    """Installs the per-thread streams, display hook and import guard once
    per process."""
    if not getattr(sys.stdout, "_atd_textport", False):
        sys.stdout = _Stream("out", sys.stdout)
    if not getattr(sys.stderr, "_atd_textport", False):
        sys.stderr = _Stream("err", sys.stderr)
    if getattr(sys.displayhook, "original", None) is None:
        _displayhook.original = sys.displayhook
        sys.displayhook = _displayhook
    if not any(getattr(f, "__name__", "") == "_OpsGuard" for f in sys.meta_path):
        sys.meta_path.insert(0, _OpsGuard)


class Console:
    """One textport's interpreter state: namespace and pending lines.

    Lines are collected until they compile, as in Python's own prompt: an
    incomplete statement (`for i in range(3):`) waits for more lines, and a
    compound statement typed one line at a time ends with an empty line. Text
    of several lines sent at once (Shift+Enter in the panel) is a block:
    Enter ends it, it runs as a module, and the value of a last expression is
    shown.
    """

    def __init__(self, bridge, ops_dir, allowed):
        import atidarla

        self.bridge = bridge
        self.blocked = None if allowed or not ops_dir else ops_dir
        self.compile = codeop.CommandCompiler()
        self.buffer = []
        self.block = False
        self.ns = {"__name__": "__console__", "__doc__": None, "__builtins__": builtins, "atidarla": atidarla}

    def push(self, text):
        """Adds `text` (one or more lines); runs the collected lines once they
        form a whole statement. Returns True while more lines are needed."""
        _local.bridge = self.bridge
        _local.blocked = self.blocked
        _local.console = self
        lines = text.split("\n")
        if len(lines) > 1:
            self.block = True
        self.buffer.extend(lines)
        source = "\n".join(self.buffer)
        block = self.block
        try:
            code = self.compile(source, FILENAME, "exec" if block else "single")
        except (OverflowError, SyntaxError, ValueError) as e:
            self._reset_buffer()
            self._write("err", "".join(traceback.format_exception_only(type(e), e)))
            return False
        if code is None:
            return True
        self._reset_buffer()
        if not block:
            self._run(code)
            return False
        flags = self.compile.compiler.flags
        tree = compile(source, FILENAME, "exec", ast.PyCF_ONLY_AST | flags, dont_inherit=True)
        last = None
        if tree.body and isinstance(tree.body[-1], ast.Expr):
            last = ast.Interactive(body=[tree.body.pop()])
        if self._run(compile(tree, FILENAME, "exec", flags, dont_inherit=True)) and last is not None:
            self._run(compile(last, FILENAME, "single", flags, dont_inherit=True))
        return False

    def _reset_buffer(self):
        self.buffer = []
        self.block = False

    def _run(self, code):
        try:
            exec(code, self.ns)
        except SystemExit:
            self._write("err", "exit() does not close the textport; Clear starts a fresh console.\n")
            return False
        except BaseException as e:
            # Leave out this frame: show the user's code only.
            tb = e.__traceback__.tb_next if e.__traceback__ is not None else None
            self._write("err", "".join(traceback.format_exception(type(e), e, tb)))
            return False
        return True

    def _write(self, stream, text):
        self.bridge.write(stream, text)


def new_console(bridge, ops_dir, allowed):
    """A fresh console (a new namespace) for the textport behind `bridge`."""
    _install_routing()
    return Console(bridge, ops_dir, allowed)


def thread_ident():
    return threading.get_ident()


def leave():
    """Forgets this thread's console (the worker is stopping)."""
    for name in ("bridge", "blocked", "console", "snap"):
        if hasattr(_local, name):
            delattr(_local, name)


# The console API, installed on the embedded `atidarla` module.


def _snapshot():
    version, text = _bridge().snapshot()
    cache = getattr(_local, "snap", None)
    if cache is None or cache[0] != version:
        cache = (version, json.loads(text))
        _local.snap = cache
    return cache[1]


def _node(name):
    if not isinstance(name, str):
        raise TypeError(f"node names are strings, not {type(name).__name__}")
    for n in _snapshot()["nodes"]:
        if n["name"] == name:
            return n
    raise ValueError(f"no node named {name!r}")


def nodes():
    """Names of the nodes in the open network, oldest first."""
    return [n["name"] for n in _snapshot()["nodes"]]


def optype(name):
    """The operator type of node `name`, such as 'img.noise'."""
    return _node(name)["type"]


def params(name):
    """Every parameter of node `name` as {parameter: value}."""
    return {k: v for k, v in _node(name)["params"]}


def par(name, param):
    """The value of parameter `param` of node `name`."""
    for k, v in _node(name)["params"]:
        if k == param:
            return v
    raise ValueError(f"node {name!r} has no parameter {param!r}")


def set_par(name, param, value):
    """Sets parameter `param` of node `name` to `value`, as one undoable
    edit in the app. Returns once the app has applied it; raises ValueError
    if the app refused it (wrong type, out of range)."""
    par(name, param)
    if isinstance(value, tuple):
        value = list(value)
    try:
        text = json.dumps(value, allow_nan=False)
    except (TypeError, ValueError):
        raise TypeError(f"{type(value).__name__} is not a parameter value") from None
    _bridge().set_par(name, param, text)


def frame():
    """The number of the frame the app cooked last."""
    return _bridge().frame()


def time():
    """The time in seconds of the frame the app cooked last."""
    return _bridge().time()


API = (nodes, optype, params, par, set_par, frame, time)


def install(module):
    for f in API:
        setattr(module, f.__name__, f)
