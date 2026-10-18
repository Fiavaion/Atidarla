"""Atidarla Python operators: declaration reader and cook host.

See docs/python-ops.md. `atd-py` loads this file twice:

* once per process as the declaration reader (`read`, `combined_hash`): it
  parses a project's `ops/*.py` with `ast` and reads the `@atidarla.op(...)`
  declarations as literals. It never compiles or runs project code;
* once per operator worker thread as the host (`load`, `cook`, `forget`),
  which runs project code. atd-py calls the host only after the user allowed
  the project's Python operators.

The parameter constructors and `op` below are installed on the embedded
`atidarla` module, so project files use them as `atidarla.Float(...)`.
"""

import ast
import copy
import hashlib
import json
import re
import sys
import threading
import traceback
import types

MAX_SOURCE = 1 << 20
FAMILIES = ("SIG", "IMG")
_NAME = re.compile(r"[a-z_][a-z0-9_]*\Z")
_OP_KEYS = ("family", "label", "summary", "params", "inputs", "state", "size", "timeout", "name")
DEFAULT_SIZE = (64, 64)
MAX_SIDE = 1024
DEFAULT_TIMEOUT = 2.0
MAX_TIMEOUT = 60.0


class DeclError(Exception):
    """A declaration error, with the line it is on when known."""

    def __init__(self, line, message):
        super().__init__(message)
        self.line = line
        self.message = message


class OpTimeout(TimeoutError):
    """Raised inside a cook that ran past its timeout (by the supervisor)."""


# ---------------------------------------------------------------------------
# Declaration API (shared by the reader and the host)
# ---------------------------------------------------------------------------


class _Param(dict):
    """A parameter, in the JSON form of atd-core's `ParamSchema`."""


class _Input(dict):
    """An input socket, in the JSON form of atd-core's `SocketSchema`."""


def _label(name):
    return " ".join(w.capitalize() for w in name.split("_") if w)


def _check_name(what, name):
    if not isinstance(name, str) or not _NAME.match(name):
        raise ValueError(f"{what} {name!r} must be lower-case letters, digits and _ (not first)")


def _is_number(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool) and v == v and abs(v) != float("inf")


def _text(what, v):
    if not isinstance(v, str):
        raise ValueError(f"{what} must be a string")
    return v


def _param(name, label, kind, default, help, **extra):
    _check_name("parameter name", name)
    p = _Param(name=name, label=_text("label", label) if label is not None else _label(name), type=kind)
    p.update(extra)
    p["default"] = default
    p["help"] = _text("help", help)
    return p


def _limits(lo, hi, default, check):
    for what, v in (("min", lo), ("max", hi)):
        if v is not None and not check(v):
            raise ValueError(f"{what} must be a number")
    if lo is not None and hi is not None and lo > hi:
        raise ValueError("min is greater than max")
    if (lo is not None and default < lo) or (hi is not None and default > hi):
        raise ValueError(f"default {default} is outside min..max")
    out = {}
    if lo is not None:
        out["min"] = lo
    if hi is not None:
        out["max"] = hi
    return out


def Float(name, label=None, default=0.0, min=None, max=None, help=""):
    if not _is_number(default):
        raise ValueError("default must be a number")
    lim = _limits(min, max, default, _is_number)
    lim = {k: float(v) for k, v in lim.items()}
    return _param(name, label, "float", float(default), help, **lim)


def Int(name, label=None, default=0, min=None, max=None, help=""):
    is_int = lambda v: isinstance(v, int) and not isinstance(v, bool)
    if not is_int(default):
        raise ValueError("default must be an integer")
    return _param(name, label, "int", default, help, **_limits(min, max, default, is_int))


def Toggle(name, label=None, default=False, help=""):
    if not isinstance(default, bool):
        raise ValueError("default must be True or False")
    return _param(name, label, "toggle", default, help)


def Menu(name, label=None, options=(), default=None, help=""):
    options = list(options) if isinstance(options, (list, tuple)) else None
    if not options or not all(isinstance(o, str) and o for o in options):
        raise ValueError("options must be a non-empty list of strings")
    if len(set(options)) != len(options):
        raise ValueError("options must be different")
    if default is None:
        default = options[0]
    if default not in options:
        raise ValueError(f"default {default!r} is not one of the options")
    return _param(name, label, "menu", default, help, options=options)


def String(name, label=None, default="", help=""):
    return _param(name, label, "string", _text("default", default), help)


def _numbers(what, v, sizes):
    if not isinstance(v, (list, tuple)) or len(v) not in sizes or not all(_is_number(x) for x in v):
        raise ValueError(f"{what} must be {' or '.join(map(str, sizes))} numbers")
    return [float(x) for x in v]


def Colour(name, label=None, default=(1.0, 1.0, 1.0, 1.0), help=""):
    rgba = _numbers("default", default, (3, 4))
    if len(rgba) == 3:
        rgba.append(1.0)
    return _param(name, label, "colour", rgba, help)


def Vec(name, label=None, default=(0.0, 0.0), help=""):
    v = _numbers("default", default, (1, 2, 3, 4))
    return _param(name, label, "vec", v, help, n=len(v))


def Input(name, wire="SIG", optional=True):
    _check_name("input name", name)
    if wire != "SIG":
        raise ValueError(f"wire {wire!r}: Python operators take SIG inputs only (IMG inputs are not supported yet)")
    if not isinstance(optional, bool):
        raise ValueError("optional must be True or False")
    return _Input(name=name, wire="SIG", optional=optional)


def op(family=None, label=None, summary="", params=(), inputs=(), state=None, size=None, timeout=None, name=None):
    """Declares a function as an operator; see docs/python-ops.md."""
    kw = dict(family=family, label=label, summary=summary, params=params, inputs=inputs,
              state=state, size=size, timeout=timeout, name=name)

    def wrap(fn):
        fn.__atidarla_op__ = kw
        return fn

    return wrap


CONSTRUCTORS = {
    "Float": Float, "Int": Int, "Toggle": Toggle, "Menu": Menu, "String": String,
    "Colour": Colour, "Vec": Vec, "Input": Input,
}


def _plain(v, where):
    """Checks a state value is plain data (copied per node, kept on reload)."""
    if v is None or isinstance(v, (bool, int, float, str)):
        return
    if isinstance(v, (list, tuple)):
        for x in v:
            _plain(x, where)
        return
    if isinstance(v, dict):
        for k, x in v.items():
            if not isinstance(k, str):
                raise ValueError(f"{where}: dict keys must be strings")
            _plain(x, where)
        return
    raise ValueError(f"{where}: {type(v).__name__} is not plain data (numbers, strings, lists, dicts)")


def declare(kw, fn_name):
    """Normalises an `op(...)` declaration (raises ValueError)."""
    kw = {k: v for k, v in kw.items() if v is not None}
    for k in kw:
        if k not in _OP_KEYS:
            raise ValueError(f"unknown argument {k}= (use {', '.join(_OP_KEYS)})")
    name = kw.get("name", fn_name)
    _check_name("operator name", name)
    family = kw.get("family")
    if family not in FAMILIES:
        raise ValueError(f"family= must be one of {', '.join(FAMILIES)}")
    label = _text("label", kw.get("label", _label(name)))
    summary = _text("summary", kw.get("summary", ""))
    params = list(kw.get("params", ()))
    for p in params:
        if not isinstance(p, _Param):
            raise ValueError("params= must list atidarla.Float(...), Int, Toggle, Menu, String, Colour or Vec")
    names = [p["name"] for p in params]
    dup = {n for n in names if names.count(n) > 1}
    if dup:
        raise ValueError(f"parameter {sorted(dup)[0]!r} is declared twice")
    inputs = list(kw.get("inputs", ()))
    for i in inputs:
        if not isinstance(i, _Input):
            raise ValueError("inputs= must list atidarla.Input(...)")
    names = [i["name"] for i in inputs]
    if len(set(names)) != len(names):
        raise ValueError("an input is declared twice")
    state = kw.get("state", {})
    if not isinstance(state, dict):
        raise ValueError("state= must be a dict of field names and initial values")
    for k, v in state.items():
        if not isinstance(k, str) or not k.isidentifier() or k.startswith("_"):
            raise ValueError(f"state field {k!r} must be a Python name not starting with _")
        _plain(v, f"state field {k!r}")
    if family == "IMG":
        size = kw.get("size", DEFAULT_SIZE)
        if (not isinstance(size, (list, tuple)) or len(size) != 2
                or not all(isinstance(s, int) and not isinstance(s, bool) and 1 <= s <= MAX_SIDE for s in size)):
            raise ValueError(f"size= must be (width, height), each 1 to {MAX_SIDE}")
        size = [size[0], size[1]]
    else:
        if "size" in kw:
            raise ValueError("size= is for IMG operators")
        size = None
    timeout = kw.get("timeout", DEFAULT_TIMEOUT)
    if not _is_number(timeout) or not 0.01 <= timeout <= MAX_TIMEOUT:
        raise ValueError(f"timeout= must be 0.01 to {MAX_TIMEOUT:g} seconds")
    return {
        "name": name, "family": family, "label": label, "summary": summary,
        "params": [dict(p) for p in params], "inputs": [dict(i) for i in inputs],
        "state": state, "size": size, "timeout": float(timeout),
    }


# ---------------------------------------------------------------------------
# Reader: no project code runs here
# ---------------------------------------------------------------------------


def _call_name(func):
    if isinstance(func, ast.Name):
        return func.id
    if isinstance(func, ast.Attribute):
        return func.attr
    return None


_LITERALS = ("declarations must be literals (numbers, strings, True/False/None, lists, tuples, "
             "dicts and atidarla.Float(...)-style parameters)")


def _literal(node):
    if isinstance(node, ast.Constant):
        if node.value is None or isinstance(node.value, (bool, int, float, str)):
            return node.value
        raise DeclError(node.lineno, _LITERALS)
    if isinstance(node, (ast.List, ast.Tuple)):
        return [_literal(e) for e in node.elts]
    if isinstance(node, ast.Dict):
        out = {}
        for k, v in zip(node.keys, node.values):
            if not (isinstance(k, ast.Constant) and isinstance(k.value, str)):
                raise DeclError(node.lineno, "dict keys in a declaration must be strings")
            out[k.value] = _literal(v)
        return out
    if (isinstance(node, ast.UnaryOp) and isinstance(node.op, (ast.USub, ast.UAdd))
            and isinstance(node.operand, ast.Constant) and _is_number(node.operand.value)):
        v = node.operand.value
        return -v if isinstance(node.op, ast.USub) else v
    if isinstance(node, ast.Call):
        name = _call_name(node.func)
        ctor = CONSTRUCTORS.get(name)
        if ctor is None:
            raise DeclError(node.lineno, f"{name or 'this'}(...) is not a parameter or input constructor; {_LITERALS}")
        args = [_literal(a) for a in node.args]
        kwargs = {}
        for k in node.keywords:
            if k.arg is None:
                raise DeclError(node.lineno, f"{name}(**...): {_LITERALS}")
            kwargs[k.arg] = _literal(k.value)
        try:
            return ctor(*args, **kwargs)
        except (TypeError, ValueError) as e:
            raise DeclError(node.lineno, f"{name}(...): {e}") from None
    raise DeclError(getattr(node, "lineno", None), _LITERALS)


def _op_decorator(dec):
    """The decorator's call if it is `op(...)`; raises if it is `op` uncalled."""
    if isinstance(dec, ast.Call) and _call_name(dec.func) == "op":
        return dec
    if _call_name(dec) == "op":
        raise DeclError(dec.lineno, "write @atidarla.op(family=...), with the parentheses")
    return None


def _read(source, filename):
    if len(source.encode("utf-8")) > MAX_SOURCE:
        raise DeclError(None, "the file is larger than 1 MB")
    try:
        tree = ast.parse(source, filename)
    except SyntaxError as e:
        raise DeclError(e.lineno, f"SyntaxError: {e.msg}") from None
    except (ValueError, RecursionError, MemoryError) as e:
        raise DeclError(None, f"cannot parse the file: {type(e).__name__}: {e}") from None
    ops = []
    for node in ast.walk(tree):
        decs = getattr(node, "decorator_list", ())
        calls = [c for c in map(_op_decorator, decs) if c is not None]
        if not calls:
            continue
        call = calls[0]
        if len(calls) > 1:
            raise DeclError(calls[1].lineno, "one @atidarla.op per function")
        if not isinstance(node, ast.FunctionDef) or node not in tree.body:
            raise DeclError(call.lineno, "@atidarla.op goes on a plain function at the top level of the file")
        if call.args:
            raise DeclError(call.lineno, "@atidarla.op takes keyword arguments only")
        kw = {}
        for k in call.keywords:
            if k.arg is None:
                raise DeclError(call.lineno, f"@atidarla.op(**...): {_LITERALS}")
            kw[k.arg] = _literal(k.value)
        try:
            decl = declare(kw, node.name)
        except ValueError as e:
            raise DeclError(call.lineno, str(e)) from None
        decl["line"] = call.lineno
        ops.append(decl)
    names = [o["name"] for o in ops]
    for o in ops:
        if names.count(o["name"]) > 1:
            raise DeclError(o["line"], f"operator {o['name']!r} is declared twice in this file")
    ops.sort(key=lambda o: o["line"])
    return ops


def _normalised(source):
    return source.replace("\r\n", "\n").encode("utf-8")


def read(source, filename):
    """JSON: {"sha256", "ops": [...]} or {"sha256", "error": {"line", "message"}}."""
    sha = hashlib.sha256(_normalised(source)).hexdigest()
    try:
        return json.dumps({"sha256": sha, "ops": _read(source, filename)})
    except DeclError as e:
        return json.dumps({"sha256": sha, "error": {"line": e.line, "message": e.message}})


def combined_hash(files):
    """SHA-256 of a project's `ops/` files: [(file name, sha256 of its text)]."""
    h = hashlib.sha256()
    for name, sha in sorted(files):
        h.update(f"{name}\0{sha}\n".encode("utf-8"))
    return h.hexdigest()


# ---------------------------------------------------------------------------
# Host: runs project code (only once the project's Python operators are allowed)
# ---------------------------------------------------------------------------

_modules = {}  # file stem -> {"ops": {name: (fn, decl)}, "error": str or None, "filename": str}
_states = {}  # node key -> (type_id, State)


class State:
    """A node's declared state; only declared fields exist."""

    __slots__ = ("_fields",)

    def __init__(self, fields):
        object.__setattr__(self, "_fields", fields)

    def __getattr__(self, name):
        try:
            return object.__getattribute__(self, "_fields")[name]
        except KeyError:
            raise AttributeError(f"state has no field {name!r}: declare it in state={{...}}") from None

    def __setattr__(self, name, value):
        fields = object.__getattribute__(self, "_fields")
        if name not in fields:
            raise AttributeError(f"state has no field {name!r}: declare it in state={{...}}")
        fields[name] = value

    def __repr__(self):
        return f"State({object.__getattribute__(self, '_fields')!r})"


class _Par:
    __slots__ = ("_values",)

    def __init__(self, values):
        object.__setattr__(self, "_values", values)

    def __getattr__(self, name):
        try:
            return object.__getattribute__(self, "_values")[name]
        except KeyError:
            raise AttributeError(f"no parameter {name!r}") from None

    def __setattr__(self, name, value):
        raise AttributeError("parameters are read-only in a cook")


class Ctx:
    __slots__ = ("time", "dt", "frame", "par", "inputs", "state", "width", "height")

    def image(self):
        """A new, zeroed RGBA8 buffer of the output size."""
        return bytearray(self.width * self.height * 4)


def thread_ident():
    import threading

    return threading.get_ident()


def _where(tb, filename):
    """The innermost traceback line in `filename`, as ' (ops/x.py, line N, in f)'."""
    frames = [f for f in traceback.extract_tb(tb) if f.filename == filename]
    if not frames:
        return ""
    f = frames[-1]
    return f" ({f.filename}, line {f.lineno}, in {f.name})"


def _error(e, filename):
    return f"{type(e).__name__}: {e}{_where(e.__traceback__, filename)}"


def _migrate(old, decl_state):
    """New state fields: old values kept where the name and type still match."""
    fields = copy.deepcopy(decl_state)
    kept, reset = [], []
    old_fields = object.__getattribute__(old, "_fields")
    for k, init in decl_state.items():
        if k in old_fields:
            if type(old_fields[k]) is type(init):
                fields[k] = old_fields[k]
                kept.append(k)
            else:
                reset.append(k)
    return State(fields), kept, reset


def load(stem, source, filename):
    """Runs a file of operators. Returns None, or the error as text."""
    old = _modules.get(stem)
    try:
        code = compile(source, filename, "exec", dont_inherit=True)
        ns = {"__name__": f"atidarla_ops.{stem}", "__file__": filename}
        with _import_lock:
            saved = sys.modules.get("atidarla")
            if _api is not None:
                sys.modules["atidarla"] = _api
            try:
                exec(code, ns)
            finally:
                if saved is None:
                    sys.modules.pop("atidarla", None)
                else:
                    sys.modules["atidarla"] = saved
        ops = {}
        for value in list(ns.values()):
            kw = getattr(value, "__atidarla_op__", None) if callable(value) else None
            if isinstance(kw, dict):
                decl = declare(kw, value.__name__)
                ops[decl["name"]] = (value, decl)
    except BaseException as e:  # noqa: BLE001 (any failure of user code is reported)
        msg = _error(e, filename)
        _modules[stem] = {"ops": old["ops"] if old else {}, "error": msg, "filename": filename}
        return msg
    _modules[stem] = {"ops": ops, "error": None, "filename": filename}
    for key, (type_id, st) in list(_states.items()):
        f, _, name = type_id[3:].partition(".")
        if f == stem and name in ops:
            new, _kept, _reset = _migrate(st, ops[name][1]["state"])
            _states[key] = (type_id, new)
    return None


def broken(stem, filename, msg):
    """The file's declarations no longer read: keep its last code and state."""
    old = _modules.get(stem)
    _modules[stem] = {"ops": old["ops"] if old else {}, "error": msg, "filename": filename}


def fail(stem, filename, msg):
    """Marks a file as failed without running it (its loading hung before)."""
    _modules[stem] = {"ops": {}, "error": msg, "filename": filename}


def unload(stem):
    _modules.pop(stem, None)


def forget(key):
    _states.pop(key, None)


def _channels(out):
    if not isinstance(out, dict):
        raise TypeError(f"a SIG operator returns a dict of channel values, not {type(out).__name__}")
    ch = []
    for k, v in out.items():
        if not isinstance(k, str):
            raise TypeError(f"channel names are strings, not {type(k).__name__}")
        if isinstance(v, bool) or not isinstance(v, (int, float)):
            try:
                v = float(v)
            except (TypeError, ValueError):
                raise TypeError(f"channel {k!r}: {type(v).__name__} is not a number") from None
        ch.append((k, float(v)))
    return ch


def _pixels(out, width, height):
    n = width * height * 4
    try:
        data = bytes(memoryview(out))
    except TypeError:
        try:
            values = list(out)
        except TypeError:
            raise TypeError(f"an IMG operator returns RGBA bytes or a list of floats, not {type(out).__name__}") from None
        if len(values) != n:
            raise ValueError(f"expected {n} values ({width}x{height} RGBA), got {len(values)}") from None
        data = bytes(max(0, min(255, int(float(v) * 255.0 + 0.5))) for v in values)
    if len(data) != n:
        raise ValueError(f"expected {n} bytes ({width}x{height} RGBA), got {len(data)}")
    return data


def cook(key, type_id, time, dt, frame, width, height, params, inputs):
    """One cook: ("sig", [(name, value)]), ("img", bytes) or ("error", text)."""
    stem, _, name = type_id[3:].partition(".")
    m = _modules.get(stem)
    filename = f"ops/{stem}.py"
    if m is None:
        return ("error", f"{filename} is not loaded")
    if m["error"] is not None:
        return ("error", f"{m['error']} (fix {filename} and save it to continue)")
    entry = m["ops"].get(name)
    if entry is None:
        return ("error", f"{filename} no longer defines an operator named {name!r}")
    fn, decl = entry
    slot = _states.get(key)
    if slot is None or slot[0] != type_id:
        slot = (type_id, State(copy.deepcopy(decl["state"])))
        _states[key] = slot
    ctx = Ctx()
    ctx.time = time
    ctx.dt = dt
    ctx.frame = frame
    ctx.par = _Par(dict(params))
    ctx.inputs = {n: dict(ch) for n, ch in inputs}
    ctx.state = slot[1]
    ctx.width = width
    ctx.height = height
    try:
        out = fn(ctx)
        if decl["family"] == "SIG":
            return ("sig", _channels(out))
        return ("img", _pixels(out, width, height))
    except OpTimeout as e:
        return ("error", f"TimeoutError: the cook took longer than its timeout of {decl['timeout']:g} s"
                         f"{_where(e.__traceback__, m['filename'])}")
    except BaseException as e:  # noqa: BLE001
        return ("error", _error(e, m["filename"]))


# This host's own `atidarla` module, with its declaration API. Every host has
# its own parameter classes, and `op` checks against them: if two hosts in one
# process shared the embedded module, the later `install` would replace the
# classes under the earlier host's project files and `params=` would be
# rejected. `load` therefore runs project code with this copy as `atidarla`,
# under a lock, because `sys.modules` is shared by every host in the process.
_api = None
_import_lock = threading.RLock()


def install(module):
    """Builds this host's `atidarla` module from the embedded one, with the declaration API."""
    global _api
    api = types.ModuleType("atidarla")
    api.__dict__.update({k: v for k, v in vars(module).items() if not k.startswith("__")})
    for name, value in CONSTRUCTORS.items():
        setattr(api, name, value)
    api.op = op
    _api = api
    # Also on the embedded module, for code that reads it directly.
    for name, value in CONSTRUCTORS.items():
        setattr(module, name, value)
    module.op = op
