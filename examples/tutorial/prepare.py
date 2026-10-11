#!/usr/bin/env python3
"""Copy the tutorial's tested files for a step into a new or existing project."""
import argparse
from pathlib import Path
import shutil


def prepare(step, destination):
    source = Path(__file__).resolve().parent
    destination = Path(destination).resolve()
    if destination == source or source in destination.parents:
        raise ValueError("Choose a project directory outside the example source.")
    destination.mkdir(parents=True, exist_ok=True)
    marker = destination / ".tutorial-step"
    if any(destination.iterdir()) and not marker.exists():
        # dre new creates only these starter files; never overwrite other user work.
        allowed = {"dre_project.yml", "reports", "dependencies.yml", ".gitignore",
                   "macros", "templates", "timings.yml"}
        if {p.name for p in destination.iterdir()} - allowed:
            raise ValueError("Destination is not a tutorial project; choose a new directory.")
        if (destination / "reports" / "sales").exists():
            raise ValueError("Destination already contains a sales report; choose a new directory.")
    if marker.exists():
        previous = int(marker.read_text().strip())
        if step != previous + 1:
            raise ValueError("Advance one step at a time, or start in a new directory.")
    elif step != 1:
        raise ValueError("Start with step 1.")
    if step == 1:
        (destination / "dre_project.yml").write_text(
            "name: first_report\ndefault_profile: sample\n", encoding="utf-8")
        for name in ("profiles.yml", "dependencies.yml"):
            shutil.copyfile(source / name, destination / name)
    steps = sorted((source / ".steps").iterdir())
    for file in steps[step - 1].rglob("*"):
        if file.is_file():
            target = destination / file.relative_to(steps[step - 1])
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(file, target)
    marker.write_text(str(step) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("step", type=int, choices=range(1, 8))
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    prepare(args.step, args.destination)
