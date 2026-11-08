#!/usr/bin/env python3
"""Rebuild the public Akamai CloudFormation template with the current Lambda source.

This source template uses AWS pseudo-parameters and must not embed private account IDs.
This command does not call AWS or deploy resources.
"""
import json
from pathlib import Path

base = Path(__file__).resolve().parent
path = base / "lodestar-akamai-watchdog.json"
template = json.loads(path.read_text(encoding="utf-8"))
source = (base / "index.py").read_text(encoding="utf-8")
template["Resources"]["Function"]["Properties"]["Code"]["ZipFile"] = source
raw = json.dumps(template, indent=2, ensure_ascii=False) + "\n"
path.write_text(raw, encoding="utf-8", newline="\n")
print("Updated canonical Akamai template using the current Lambda source.")
