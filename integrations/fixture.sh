#!/usr/bin/env bash
# A two-file Python repository for the integration tests: fixture.sh DIR
set -euo pipefail
mkdir -p "$1/src"
printf 'class Point:\n    def norm(self):\n        return 1\n\n\ndef make_point():\n    """Build the origin point."""\n    return Point()\n\n\ndef use_it():\n    p = make_point()\n    return p.norm()\n' > "$1/src/geo.py"
printf 'from src.geo import make_point\n\n\ndef main():\n    return make_point().norm()\n' > "$1/src/app.py"
