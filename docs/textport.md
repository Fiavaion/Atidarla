# Textport

The Textport is a Python console inside Lasair. Type a line of Python, press Enter, and see
the result straight away. It can read the open project (nodes, parameter values, the current
frame and time) and change parameters, and every change it makes can be undone like a change
made by hand. PRD: FR-P5 ("Textport/REPL bound to the `lasair` Python module"), §9 M2.

## Opening it

Choose **View > Textport**. The Textport opens as a tab in the bottom area, next to Timeline.
It is closed when Lasair starts; its **Close** button closes it again. Like any tab, you can
drag it to another area.

## Typing

| Key | What it does |
|---|---|
| Enter | Runs what you typed |
| Shift+Enter | Starts a new line without running |
| Up / Down | Brings back earlier entries (when the input is one line) |

It works like Python's own prompt:

- **Expressions show their value.** `2 + 3` shows `5`; `'abc'` shows `'abc'` (the value's
  `repr`). `None` shows nothing. `_` holds the last value shown.
- **`print` and errors show too.** Output from `print` is shown as it happens, even while a
  long statement is still running. Errors show a traceback, in red; the console carries on.
- **Unfinished statements wait for more.** After `for i in range(3):` the prompt changes from
  `>>>` to `...` and waits for the body. Type the body lines, then press Enter on an empty line
  to run it, as in Python. An open bracket waits the same way.
- **Blocks run on Enter.** If you type several lines with Shift+Enter and then press Enter, the
  whole block runs at once, without an empty line, and the value of its last line is shown if
  that line is an expression.
- **Indent with spaces.** Tab moves to the next control, as everywhere else in the app.

## Buttons

| Button | What it does |
|---|---|
| Clear | Empties the output. Your variables stay. |
| Reset | Starts a fresh namespace: every variable and import is forgotten. |
| Stop | Stops the running code by raising `KeyboardInterrupt` in it. Code waiting inside a long `time.sleep` stops when the sleep ends. |
| Close | Closes the Textport tab. Code still running carries on. |

While code runs, the Textport shows "running…". You can type and press Enter meanwhile; entries
run one after another.

## Variables stay until the project changes

Variables, functions and imports stay from one entry to the next. When you open another project
or start a new one (File > New, File > Open), the Textport starts afresh and says so. Saving
does not reset it.

## The `lasair` API

The name `lasair` is ready to use; you do not need to import it.

| Call | Returns |
|---|---|
| `lasair.nodes()` | The names of the nodes in the patch, oldest first, as a list of strings. |
| `lasair.optype(name)` | The operator type of a node, such as `'img.noise'`. |
| `lasair.params(name)` | Every parameter of a node, as a dictionary of name to value. |
| `lasair.par(name, param)` | One parameter's value. |
| `lasair.set_par(name, param, value)` | Sets a parameter. Returns `None` once the change is made. |
| `lasair.frame()` | The number of the frame the app cooked last. |
| `lasair.time()` | The time of that frame, in seconds. |
| `lasair.version()` | Lasair's version. |
| `lasair.platform()` | The operating system and processor, as a dictionary. |

Examples:

```python
>>> lasair.nodes()
['noise1', 'kaleidoscope1', 'level1', 'drift', 'spin']
>>> lasair.par('noise1', 'scale')
3.0
>>> lasair.set_par('noise1', 'scale', 7.5)
>>> for n in lasair.nodes():
...     print(n, lasair.optype(n))
...
```

### Values

Numbers come back as `int` or `float`, toggles as `True` or `False`, menus and text as strings,
and colours and vectors as lists, such as `[1.0, 0.5, 0.0, 1.0]`. A parameter you never changed
shows its default. A number that follows an expression shows the expression's latest result.
A parameter bound to a channel shows the value it holds underneath the binding.

### Setting parameters

`set_par` asks the app to make the change, and the app makes it between two frames, exactly as
if you had typed the value into the Parameters panel:

- **It can be undone.** Each `set_par` is one step in **Edit > Undo**, named
  "Textport: node.param".
- **It replaces an expression or binding** on that parameter with the plain value, as typing a
  value in the Parameters panel does.
- **It is checked.** A value of the wrong type, or outside the parameter's range, is refused
  with a `ValueError`, and nothing changes. A value Python cannot turn into a parameter value
  (an arbitrary object) raises `TypeError`.
- **It waits for the app** (normally one frame), so `par` straight afterwards shows the new
  value. If the app does not get to it within 5 seconds, `set_par` raises `TimeoutError`; the
  change may still be made afterwards.

A loop that calls `set_par` many times makes as many undo steps.

### Where the API works

The API reads the project only from the Textport. Calling it from a Python operator raises
`RuntimeError`. Threads you start yourself from the Textport cannot use it either, and their
`print` output does not appear in the Textport.

## Your show keeps running

Code in the Textport runs on its own background thread, never on the thread that draws and
cooks the patch. A slow statement, `time.sleep(10)` or an endless loop does not stop the patch,
the viewer or your outputs: frames keep coming, and the Textport shows "running…" until the
code finishes (or until you press Stop). The Textport also runs apart from Python expressions
and Python operators, so its code does not delay them.

## Trust and project code

The Textport runs only what you type, so it does not ask for permission first. It is still full
Python: anything you type can do what your user account can do (read and change files, use the
network).

It does not run a project's own Python code by itself. It never loads the files in the
project's `ops/` folder, and while the project's Python operators are not allowed (see
[Python custom operators, Trust](python-ops.md#trust-running-code-from-a-project)), it refuses
to import a module from that folder, even if you add the folder to `sys.path`. Once you allow
them (File > Allow Python Operators…), the Textport starts afresh and such imports work.

This is a guard against running a project's code by accident, not a sandbox: code you type
yourself can still read a file and run it.

## Limits

- The output keeps the latest 5000 lines and the input history the latest 500 entries. Neither
  is saved with the project.
- The API reads nodes and parameters only. Channels, images and geometry are not available
  yet, and nodes cannot be added, removed or wired from the Textport.
- `exit()` and `quit()` do not close anything; use Reset for a fresh namespace.
