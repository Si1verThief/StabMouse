#!/usr/bin/env python3
"""Plot the output of `stabmouse-replay compare`.

The point of the whole bench: every variant here saw *identical* input, so
differences between the curves are attributable to the filters rather than to
whether your hand was steadier on one attempt.

    stabmouse-replay compare --input strokes.tsv --variants variants.toml --out out.csv
    python3 plot.py out.csv

Writes out.csv.png beside the input unless --out is given.
"""

import argparse
import csv
import math
from collections import defaultdict

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt


def load(path):
    variants = defaultdict(lambda: defaultdict(list))
    with open(path, newline="") as f:
        for row in csv.DictReader(f):
            v = variants[row["variant"]]
            v["t"].append(int(row["t_us"]) / 1e6)
            v["x"].append(float(row["out_x_mm"]))
            v["y"].append(float(row["out_y_mm"]))
            v["p"].append(float(row["pressure"]))
            v["down"].append(row["down"] == "1")
            v["dx"].append(float(row["out_dx_mm"]))
            v["dy"].append(float(row["out_dy_mm"]))
    return variants


def stroke_segments(v):
    """Contiguous runs where the button was held."""
    runs, start = [], None
    for i, d in enumerate(v["down"]):
        if d and start is None:
            start = i
        elif not d and start is not None:
            runs.append((start, i))
            start = None
    if start is not None:
        runs.append((start, len(v["down"])))
    return runs


def speed(v):
    out = []
    t = v["t"]
    for i in range(len(t)):
        dt = (t[i] - t[i - 1]) if i else 0.0
        d = math.hypot(v["dx"][i], v["dy"][i])
        out.append(d / dt if dt > 0 else 0.0)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("csv")
    ap.add_argument("--out")
    args = ap.parse_args()

    variants = load(args.csv)
    if not variants:
        raise SystemExit("no rows found")

    names = list(variants)
    n = len(names)
    fig, axes = plt.subplots(n, 3, figsize=(16, 3.1 * n), squeeze=False)
    fig.suptitle(f"StabMouse variant comparison — {args.csv}", y=0.998)

    # Shared path extents so shapes are visually comparable, not auto-scaled apart.
    all_x = [x for v in variants.values() for x in v["x"]]
    all_y = [y for v in variants.values() for y in v["y"]]
    pad = 2.0
    xlim = (min(all_x) - pad, max(all_x) + pad)
    ylim = (max(all_y) + pad, min(all_y) - pad)  # screen orientation

    for r, name in enumerate(names):
        v = variants[name]
        runs = stroke_segments(v)

        ax = axes[r][0]
        ax.plot(v["x"], v["y"], color="#c8c8d2", lw=0.7, zorder=1)
        for a, b in runs:
            ax.plot(v["x"][a:b], v["y"][a:b], color="#18181f", lw=1.4, zorder=2)
        ax.set_xlim(*xlim)
        ax.set_ylim(*ylim)
        ax.set_aspect("equal")
        ax.set_title(f"{name} — path", fontsize=10)
        ax.set_xlabel("mm")

        ax = axes[r][1]
        ax.plot(v["t"], v["p"], color="#6ea8fe", lw=0.8)
        for a, b in runs:
            ax.axvspan(v["t"][a], v["t"][b - 1], color="#6ea8fe", alpha=0.07)
        ax.set_ylim(-0.02, 1.02)
        # Skip the first sample of each stroke: pressure steps 0 -> min_pressure
        # there, which is the floor being applied rather than filter behaviour, and it
        # would dominate the figure. Matches what `compare` reports.
        worst = max(
            (
                abs(v["p"][i] - v["p"][i - 1])
                for i in range(1, len(v["p"]))
                if v["down"][i] and v["down"][i - 1]
            ),
            default=0.0,
        )
        ax.set_title(f"{name} — pressure (worst jump {worst:.4f})", fontsize=10)
        ax.set_xlabel("s")

        ax = axes[r][2]
        ax.plot(v["t"], speed(v), color="#8a8a93", lw=0.7)
        ax.set_title(f"{name} — output speed (mm/s)", fontsize=10)
        ax.set_xlabel("s")

    fig.tight_layout()
    out = args.out or (args.csv + ".png")
    fig.savefig(out, dpi=130)
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
