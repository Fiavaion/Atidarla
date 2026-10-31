# Soak on macOS, 60 minutes (PT-10 smoke test)

PT-10 is judged on the Windows lab image (RTX 4070) over 24 hours. This is the 60-minute macOS run
of the same command, to prove `atidarla soak` and look for leaks and stalls early.

```sh
caffeinate -i atidarla soak examples/soak-show.atidarla --minutes 60 --warmup 120 --sample 10 --cycle-seconds 8 --out <dir>
```

- Machine: Apple M5 Pro, 48 GB, macOS 26.6.2, Metal. Release build of commit `8ebfd38`
  (`atidarla soak` as committed).
- Patch: `examples/soak-show.atidarla`: the Stage deck of `stage-deck` (3 layers × 5 columns,
  120 BPM clock, fades and wipes) plus a 400,000-particle system with bloom and an image
  feedback loop as the fifth column, 1280×720. Headless, every node cooks every frame.
- Stage: the next column launched every 8 s (449 launches per run).

## Result: pass on an idle machine

| | Idle run (01:18 UTC) | Busy run (00:18 UTC) |
|---|---:|---:|
| **PT-10** | **pass** | FAIL (frame spikes) |
| Frames (after warm-up) | 215,717 (208,543) | 215,425 (208,318) |
| RSS at start / after warm-up / peak / end (MB) | 38.9 / 66.0 / 66.2 / 47.5 | 39.4 / 64.9 / 65.2 / 47.1 |
| **RSS growth after warm-up** | **−18.5 MB** | −17.8 MB |
| Mean frame | 6.91 ms | 7.00 ms |
| 99th percentile | 20.1 ms | 18.8 ms |
| **Worst frame** | **43.6 ms** (at 0:52:37) | 86.8 ms (at 0:27:16) |
| Worst CPU cook | 24.7 ms | 28.9 ms |
| Worst interval | 61.0 ms | 86.8 ms |
| Frames over 50 ms | 0 | 12 |

Per-window samples: [`soak-mac-60min-idle.csv`](soak-mac-60min-idle.csv),
[`soak-mac-60min-busy.csv`](soak-mac-60min-busy.csv).

The busy run overlapped with release and debug builds, clippy, the test suites (GPU tests
included), app smoke runs on the same GPU and a DMG build, all started on the same machine. Its
spikes fell while that work ran, and most have a small CPU cook, so the time went to waiting for
a shared GPU. It is kept as evidence that the measurement catches stalls. The idle run, with
nothing else running, passes.

## Observations

- **No leak.** RSS is flat at about 65 MB after warm-up, with 449 launches and 208,000 frames;
  about 18 minutes in, macOS reclaims about 18 MB (the drop to 47 MB, in both runs) and RSS
  stays flat after that. GPU allocations on Apple silicon are mostly not in RSS (docs/soak.md);
  the flat frame times also point to no GPU-side growth.
- **The margin under 50 ms is modest.** The mean is 7 ms, but about 1 frame in 100 takes
  20–45 ms (the 99th percentile is 20 ms). These frames mostly have a small CPU cook, so the time
  is spent waiting for the GPU. The CSV windows are 10 s, so it cannot tell whether they follow
  launches (every window has one or two). Worth profiling before the 24-hour run on the lab
  image, where the app also presents.
- Interval (wall time between frame starts) peaked at 61 ms once in the idle run with a 32 ms
  frame: an OS scheduling delay, not the show.

## Still to do for PT-10

The 24-hour run on the NCAD Windows lab image (RTX 4070):
`atidarla soak examples/soak-show.atidarla --hours 24 --warmup 300 --cycle-seconds 8 --out soak-24h`.
