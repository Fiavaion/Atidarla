# 10. Shows and reliability

This chapter is about taking a patch from your laptop to a stage, a gallery or a degree show,
and keeping it running.

## Saving and project folders

Lasair saves a **project folder** that ends in `.lasair`. Everything in it is plain text
with stable IDs, written in a fixed order. That makes projects easy to keep in Git and to
compare ([chapter 11](11-collaboration.md)).

| File or folder | What it holds |
|---|---|
| `project.toml` | Name, description, resolution, AI settings |
| `networks/main.toml` | The patch: nodes, wires, parameters |
| `timeline.toml` | The Timeline (only if you used it) |
| `mappings.toml` | MIDI and OSC mappings (only if you made any) |
| `agent.toml` | Show Lock settings (only if you made one) |
| `media/` | Movies, sounds and images the project uses |
| `kernels/`, `ops/`, `fixtures/`, `batch/`, `comfy/`, `packs/` | Your kernels, Python operators, DMX fixture types, batch wedges, imported ComfyUI workflows, and the Packs the project uses |

Use **File, Save** (Cmd/Ctrl+S), **Save As** (Cmd/Ctrl+Shift+S), **Open Project** (Cmd/Ctrl+O)
and **New Project** (Cmd/Ctrl+N). The app asks first before it replaces a project that has unsaved work.

Two checks you can run on a project from a terminal:

```sh
lasair check examples/stage-deck.lasair          # loads, validates, is in canonical form
lasair check --gpu examples/stage-deck.lasair    # also cooks every node for a second
lasair fmt myshow.lasair                         # rewrite in canonical form after editing by hand
```

## Exporting a show

A show that uses files from all over your disk will break when you move it to another
computer. **Show export** fixes that. It makes one portable folder with every file the show
needs copied inside, and every path rewritten to be relative.

```sh
lasair export-show shows/opening.lasair dist/ --zip
# dist/opening.lasair/       the bundle (a project folder)
# dist/opening.lasair.zip    the same, zipped

lasair verify-show dist/opening.lasair --gpu     # run this on the receiving computer
```

What it does:

- Loads and checks the project first. The original is never changed.
- Copies every file named by a node from outside the project into `media/`.
- **Stops and lists every missing file** (with its node and parameter) rather than making a
  broken bundle.
- Writes `bundle.toml` listing every file with a SHA-256 fingerprint and every node that
  needs a capability the other computer may not have (for example Spout or Syphon), with
  what stands in for it.
- **Model files are never put in a bundle.** The receiving computer downloads them through the
  Models window.

`verify-show` checks that every path is relative and exists with exactly the right capital
letters (Windows and macOS ignore case but some systems do not), that every fingerprint
matches, and that the project loads. Details: [`docs/show-export.md`](../show-export.md).

Not covered: file paths written *inside* shader or Python files. Keep those files in the
project folder.

## Kiosk mode and the supervisor

For an installation or a performance, you want the app full screen, with no menus, and
restarted automatically if anything goes wrong.

```sh
lasair run examples/audio-reactive-feedback.lasair --kiosk
```

**Kiosk mode** opens the app full screen, with no window frame and no cursor. It shows only the
Viewer, with no menu and no editor panels. Output windows from Window nodes open as usual.

`lasair run` is the **supervisor**. It starts the app and watches it. It restarts the app if:

- the app **crashes** (exits with an error or is killed), or
- the app **freezes**: it stops giving a heartbeat for 15 seconds (the first heartbeat is allowed
  90 seconds, for loading the project and compiling shaders).

If you quit the app normally, the supervisor does not restart it. If the app fails too many
times in a row (10 by default), the supervisor gives up. The wait between restarts doubles each
time, from 1 second up to 60. A run that lasts a minute counts as healthy. Every launch, failure
and restart is logged with a time in `<project>/.cache/restarts.log`.

Options: `--max-restarts`, `--restart-delay`, `--heartbeat-timeout` and `--startup-grace`
(chapter 13). Details and limits: [`docs/run.md`](../run.md). The supervisor cannot see a hang
inside a background worker (the picture keeps drawing and the heartbeat continues), or a window
the system has stopped drawing.

You can also start the app yourself: `lasair-app project.lasair --kiosk`.

## Crash isolation and the Output Governor

Two design rules keep a show alive:

- **Slow things run elsewhere.** AI models run in a separate AI Host program. Python, mesh
  building, file and network access run on worker threads. The picture is never made to wait
  for them. If a host crashes, the supervisor inside the app restarts it and the picture
  carries on.
- **The Output Governor** keeps AI nodes producing frames at the project's frame rate even when
  the model is slower (`hold`, `blend`, `flow` or `passthrough`; see
  [chapter 8](08-ai.md#the-output-governor)).

Python expression results arrive one frame late for the same reason.

## Soak testing

A **soak test** runs a show for hours to find memory leaks and slow-downs. The target is a 24
hour run with no leak over 100 MB and no frame spike over 50 ms. The command that will run it
(`lasair soak`) is planned for the last milestone and is **not built yet**. Today you can run
your own show under `lasair run` and watch it.

Lasair also keeps its own checks on frame-time memory use: the work done on every frame is
tested to allocate nothing after warm-up.

## Before a show: a checklist

1. Export the show with `export-show` and run `verify-show --gpu` on the show computer.
2. Download any AI models there first (the venue may not have internet).
3. Decide Sovereign mode: leave it on unless you need the cloud, and set a spend cap if you do.
4. Make a Show Lock allow-list ([chapter 9](09-assistant-agents.md#show-lock)) if people other
   than you may touch the controls.
5. Map your controllers ([chapter 7](07-live-io.md#midi-and-osc)) and save.
6. Start with `lasair run ... --kiosk`.
7. Keep a copy of the working project.
