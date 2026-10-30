# Supervised run (`atidarla run`, FR-H2)

`atidarla run` keeps the app running on a show machine: it launches `atidarla-app` on a project,
restarts it when it crashes or freezes, and stops it on Ctrl-C. It behaves the same on Windows
and macOS (Linux too).

```sh
atidarla run examples/audio-reactive-feedback.atidarla --kiosk
```

| Option | Meaning |
|---|---|
| `<project>` | Project folder (`*.atidarla`). |
| `--kiosk` | Show mode: the app opens full screen with no window frame, shows only the viewer (no menu, no editor panels) and hides the cursor. Output windows of the project (`cmp.window`, `docs/outputs.md`) open as usual. |
| `--max-restarts N` | Restarts in a row before giving up (default 10; 0 never restarts). A run that lasts a minute counts as healthy and resets the count. |
| `--restart-delay S` | Seconds before the first restart (default 1). Each restart in a row doubles it, up to 60 s. |
| `--heartbeat-timeout S` | Seconds without a heartbeat before the app is killed and restarted (default 15; 0 turns the watch off). |
| `--startup-grace S` | Seconds the app gets from launch to its first heartbeat (default 90): project load and shader compiles come first. |

The app binary is looked up as `atidarla-app` (`.exe` on Windows) beside the `atidarla`
executable, or at `$ATD_APP`.

## What counts as a failure

- The app exits with a non-zero status or is killed by a signal: restarted after the back-off.
- The heartbeat stops changing for `--heartbeat-timeout` seconds (a frozen or deadlocked UI):
  the app is killed and restarted.
- The app exits with status 0 (the operator quit it): not restarted; `run` exits 0.
- Ctrl-C or SIGTERM on `run`: the app gets 3 s to exit, then is killed; `run` exits 0.
- More failures in a row than `--max-restarts`: `run` exits non-zero.

Each launch, failure, restart and stop is printed and appended with a UTC timestamp to
`<project>/.cache/restarts.log` (`.cache/` is gitignored).

## Heartbeat

`run` starts the app with `--heartbeat <project>/.cache/heartbeat`. The UI thread only
increments a counter once per frame; a separate thread writes `<beats> <frames>` to that file
(written beside it and renamed) once a second, and only if frames were drawn since the last
write. The supervisor polls the file and treats any change of its content as a beat, so no
clock is compared. A frozen UI thread stops the beats; the render thread never waits on the
disk (CLAUDE.md rule 1). An idle show still draws twice a second, so it keeps beating.

Not covered: a hang inside a worker (the UI keeps drawing and beating) and a window the system
has stopped drawing (a minimised or covered window can pause repaints; kiosk mode is full
screen). `--heartbeat-timeout 0` turns the watch off for such setups.

Running the app by hand: `atidarla-app project.atidarla --kiosk --heartbeat beat.txt`.
