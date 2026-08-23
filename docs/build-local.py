#!/usr/bin/env python3
"""Build the docs site the way CI does.

`zola build` on its own leaves `public/search-index.json` stale, so the search
dialog would query the previous build's prose. This runs both steps in order.

Search itself needs a real server, because `fetch` refuses a `file://` URL —
use `zola serve` from this directory to exercise the dialog.

Usage:  python3 build-local.py
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

here = Path(__file__).resolve().parent
public = here / "public"


def run(*args: str) -> None:
    result = subprocess.run(args, cwd=here)
    if result.returncode != 0:
        raise SystemExit(result.returncode)


run("zola", "build")
# Reads the rendered pages, so it has to follow the build.
run(sys.executable, "search-index.py", "public")

print(f"\nBuilt: {public}\nServe: zola serve --root {here}")
