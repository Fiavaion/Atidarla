# Bench

`atidarla bench --out bench/results/<machine>.json` runs the standard performance suite (PRD §8, FR-H4) and writes `atidarla-bench/1` JSON. M0 measures start-up only (`startup.registry`, `startup.gpu_device`); each milestone adds the PT-n cases it is accountable for. Results are git-ignored; commit reference runs deliberately with `git add -f`.
