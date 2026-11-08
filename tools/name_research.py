#!/usr/bin/env python3
"""Name-collision research for the project name: for each candidate, query the public registries a
developer tool collides with (crates.io, PyPI, npm, GitHub repositories and accounts) and record the
raw evidence. Homebrew and domain/trademark checks are done separately (see NAMING.md): this script
never claims more than the registries answered.

usage: name_research.py [--out names.json] CANDIDATE...
"""
from __future__ import annotations

import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

UA = "project-name-research/1.0"


def get(url: str) -> tuple[int, dict | list | None]:
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            body = r.read()
            try:
                return r.status, json.loads(body)
            except ValueError:
                return r.status, None
    except urllib.error.HTTPError as e:
        return e.code, None
    except (urllib.error.URLError, TimeoutError) as e:
        return 0, {"error": str(e)}


def gh(path: str):
    """GitHub API via the authenticated `gh` CLI; None when the call failed (not "no result")."""
    r = subprocess.run(["gh", "api", path], capture_output=True, text=True)
    if r.returncode != 0:
        return None
    try:
        return json.loads(r.stdout)
    except ValueError:
        return None


def research(name: str) -> dict:
    out: dict = {"name": name}
    code, data = get(f"https://crates.io/api/v1/crates/{name}")
    out["crates.io"] = {"status": code, "taken": code == 200,
                        "detail": data and data.get("crate", {}).get("description", "")[:100] if code == 200 else None,
                        "downloads": data and data.get("crate", {}).get("downloads") if code == 200 else None}
    code, data = get(f"https://pypi.org/pypi/{name}/json")
    out["pypi"] = {"status": code, "taken": code == 200, "detail": (data or {}).get("info", {}).get("summary", "")[:100] if code == 200 else None}
    code, data = get(f"https://registry.npmjs.org/{name}")
    out["npm"] = {"status": code, "taken": code == 200, "detail": (data or {}).get("description", "")[:100] if code == 200 else None}
    repos = gh(f"search/repositories?q={name}+in:name&per_page=5")
    exact = [r for r in (repos or {}).get("items", []) if r["name"].lower() == name.lower()]
    top = None
    if repos and repos.get("items"):
        # the search is ranked by best match; find the most-starred exact-name repo among the top 30
        more = gh(f"search/repositories?q={name}+in:name&sort=stars&order=desc&per_page=30")
        exact = [r for r in (more or {}).get("items", []) if r["name"].lower() == name.lower()]
        if exact:
            top = {"repo": exact[0]["full_name"], "stars": exact[0]["stargazers_count"], "description": (exact[0].get("description") or "")[:100]}
    out["github"] = {"repos_matching": (repos or {}).get("total_count"), "exact_name_top": top}
    user = gh(f"users/{name}")
    # the users endpoint may be unavailable to a restricted token: then the answer is "unverified",
    # never "free"
    out["github_account"] = {"taken": bool(user and user.get("login")) if user is not None else None,
                             "type": user.get("type") if user else None, "verified": user is not None}
    time.sleep(1.0)  # be polite to the registries
    return out


def main() -> int:
    args = sys.argv[1:]
    out_path = None
    if "--out" in args:
        i = args.index("--out")
        out_path = args[i + 1]
        del args[i : i + 2]
    results = [research(n) for n in args]
    for r in results:
        print(f"{r['name']:12} crates={'T' if r['crates.io']['taken'] else '-'} pypi={'T' if r['pypi']['taken'] else '-'} npm={'T' if r['npm']['taken'] else '-'} "
              f"gh_repos={r['github']['repos_matching']} top={r['github']['exact_name_top']} account={'T' if r['github_account']['taken'] else ('-' if r['github_account']['verified'] else '?')}")
    if out_path:
        json.dump({"date": time.strftime("%Y-%m-%d"), "results": results}, open(out_path, "w"), indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
