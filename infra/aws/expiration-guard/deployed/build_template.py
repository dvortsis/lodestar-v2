#!/usr/bin/env python3
"""Rebuild the public AWS expiration-guard CloudFormation template with the current Lambda source.

The template inlines index.py as the function's ZipFile. This command does not call AWS
or deploy resources. Check with: python3 build_template.py && git diff --exit-code
"""
import json
from pathlib import Path

base = Path(__file__).resolve().parent
path = base / "template.json"
template = json.loads(path.read_text(encoding="utf-8"))
source = (base / "index.py").read_text(encoding="utf-8")
template["Resources"]["Function"]["Properties"]["Code"]["ZipFile"] = source
raw = json.dumps(template, indent=2, ensure_ascii=False) + "\n"
path.write_text(raw, encoding="utf-8", newline="\n")
print("Updated canonical AWS expiration-guard template using the current Lambda source.")
