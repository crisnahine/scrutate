"""Parse the Scoop and winget manifests, check them against the published schemas, and refuse the
all-zero sha256 placeholders (in the Homebrew formula too) unless --draft is given.

usage: uvx --with jsonschema --with pyyaml python packaging/validate_manifests.py [--draft]
"""
import glob
import json
import os
import re
import sys
import urllib.request

import jsonschema
import yaml

W = os.path.dirname(os.path.abspath(__file__))
WG = "https://raw.githubusercontent.com/microsoft/winget-cli/master/schemas/JSON/manifests/v1.12.0/manifest.{}.1.12.0.json"
SCHEMAS = {"scoop": "https://raw.githubusercontent.com/ScoopInstaller/Scoop/master/schema.json",
           "version": WG.format("version"), "installer": WG.format("installer"), "defaultLocale": WG.format("defaultLocale")}


def fetch(url):
    with urllib.request.urlopen(url, timeout=30) as r:
        return json.load(r)


draft = "--draft" in sys.argv[1:]
bad = 0
for p in [os.path.join(W, "homebrew/scrutate.rb"), os.path.join(W, "scoop/scrutate.json")] + sorted(glob.glob(os.path.join(W, "winget/**/*.yaml"), recursive=True)):
    zeros = len(re.findall(r"\b0{64}\b", open(p).read()))
    if zeros:
        print(f"{'placeholder' if draft else 'PLACEHOLDER HASH'}: {os.path.relpath(p, W)} has {zeros} all-zero sha256 value(s)")
        bad += 0 if draft else zeros
docs = [("scoop", os.path.join(W, "scoop/scrutate.json"), json.load(open(os.path.join(W, "scoop/scrutate.json"))))]
print("syntax ok: scoop/scrutate.json (JSON)")
for p in sorted(glob.glob(os.path.join(W, "winget/**/*.yaml"), recursive=True)):
    d = yaml.safe_load(open(p))
    print(f"syntax ok: {os.path.relpath(p, W)} (YAML)")
    docs.append((d["ManifestType"], p, d))
for kind, p, d in docs:
    schema = fetch(SCHEMAS[kind])
    errs = list(jsonschema.Draft7Validator(schema).iter_errors(d)) if kind != "scoop" else list(jsonschema.validators.validator_for(schema)(schema).iter_errors(d))
    for e in errs:
        print(f"SCHEMA ERROR {os.path.relpath(p, W)}: {list(e.path)} {e.message}")
    print(f"schema {'ok' if not errs else 'FAILED'}: {os.path.relpath(p, W)} against {SCHEMAS[kind]}")
    bad += len(errs)
sys.exit(1 if bad else 0)
