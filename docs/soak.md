# Soak test (`lasair soak`, PT-10)

`lasair soak` runs a project headless for hours and checks PT-10: after a warm-up, the
process's resident memory grows by no more than **100 MB** and no frame takes longer than
**50 ms**. It behaves the same on Windows and macOS (Linux too).

```sh
lasair soak examples/soak-show.lasair --hours 24 --warmup 300 --cycle-seconds 8 --out soak-24h
```

| Option | Meaning |
|---|---|
| `<project>` | Project folder (`*.lasair`). |
| `--hours H`, `--minutes M`, `--seconds S` | How long to run; the three add up. Without any of them the run lasts 24 hours. |
| `--warmup S` | Seconds at the start that are not judged (default 120): shader compiles, pools filling, caches warming. The run must be longer than this. |
| `--cycle-seconds S` | Launch the next column of the Stage deck every S seconds (wrapping), as a performer would. Without it the deck plays what the project saved. |
| `--sample S` | Seconds per sample window (default 10): one memory reading and one CSV row. |
| `--out DIR` | Folder for `soak.md` and `soak.csv` (default `soak`, created if missing). |

The exit status is 0 when PT-10 passes and non-zero when it fails, so a lab script can gate on
it.

## What it does

The project cooks at 60 frames a second as under `lasair serve` (every node, every frame, not
only what a viewer shows, so it is a heavier load than the app). For each frame it measures:

- **cook**: the CPU time of the engine's cook (graph evaluation and GPU submission);
- **frame time**: the cook plus waiting for the GPU to finish the frame. Headless there is no
  present, so this is the frame's whole cost. PT-10's 50 ms applies to it;
- **interval**: the wall time between frame starts. It includes the 60 fps pacing and the OS
  scheduler and is reported, not judged.

Resident memory (RSS) is read on its own thread (`/proc` on Linux, `ps` on macOS, `tasklist` on
Windows), never on the frame loop. Growth is measured from the first reading after warm-up to
the reading at the end; the peak is reported too. Frame statistics are kept in constant memory
(a histogram for the 99th percentile), so a 24-hour run does not grow by itself, and CSV rows are
written and flushed as the run goes by a writer thread, so a crash keeps what was measured.

## The report

`soak.md` has the verdict, the platform and GPU, memory at start, after warm-up, peak and end,
and the mean, 99th percentile and worst frame (with when it happened), the worst cook and
interval, the number of frames over 50 ms, the Stage launches, and any node errors at the end.
`soak.csv` has one row per window:

```
elapsed_s,phase,rss_mb,frames,mean_frame_ms,max_frame_ms,max_cook_ms,max_interval_ms,spikes,launches
```

## Notes

- On macOS, most of what the GPU driver allocates for textures and buffers does not show in
  RSS (the soak patch's textures and particle buffers alone are larger than its measured RSS):
  Metal allocations count in the process footprint instead. A GPU-side leak would show as rising
  frame times or a driver error rather than in this column; watch Activity Monitor's Memory
  column (the footprint) on long runs. On Windows the dedicated VRAM of a discrete GPU is not in
  RSS either.
- Run soaks on an idle machine: builds, browsers and other GPU work cause frame spikes that are
  not the show's.
- PT-10 is judged on the Windows lab image (RTX 4070). Shorter runs on other machines are useful
  smoke tests; `docs/milestones/m6/` keeps their reports.
- `examples/soak-show.lasair` is the reference patch: the Stage deck of `stage-deck` with a
  400,000-particle system and an image feedback loop as a fifth column.
