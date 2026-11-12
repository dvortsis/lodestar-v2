#!/usr/bin/env python3
"""Checks the Unraid Community Applications templates in this directory.

Run: python3 packaging/unraid/validate.py
Each template must be well-formed XML and carry the fields CA requires.
"""
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

REQUIRED = ["Name", "Repository", "Network", "Support", "Project",
            "Overview", "Category", "Icon", "TemplateURL"]
CONFIG_ATTRS = ["Name", "Target", "Default", "Mode", "Description", "Type", "Display", "Required"]
TEMPLATES = {
    "lodestar.xml": {"webui": True, "ports": {"8090", "6881"}, "paths": {"/data"}},
    "lodestar-gluetun.xml": {"webui": True, "extra": "--network=container:gluetun", "paths": {"/data"}},
    "lodestar-worker.xml": {"webui": False, "vars": {"LODESTAR_JOIN", "LODESTAR_TOKEN"}, "requires": "v0.6"},
}
NETWORK_CONTAINER = re.compile(r"--network=container:(\S+)")


def check(path: Path, rules: dict) -> list[str]:
    errors = []
    try:
        root = ET.parse(path).getroot()
    except ET.ParseError as e:
        return [f"not well-formed: {e}"]
    if root.tag != "Container" or root.get("version") != "2":
        errors.append('root must be <Container version="2">')
    for tag in REQUIRED:
        el = root.find(tag)
        if el is None or not (el.text or "").strip():
            errors.append(f"<{tag}> missing or empty")
    icon = (root.findtext("Icon") or "")
    if not icon.endswith("packaging/icons/lodestar-256.png"):
        errors.append("Icon must point at packaging/icons/lodestar-256.png")
    webui = root.findtext("WebUI") or ""
    if rules["webui"] and "8090" not in webui:
        errors.append("WebUI must target 8090")
    if rules.get("ports") and "[PORT:8090]" not in webui:
        errors.append("WebUI must use [PORT:8090]")
    extra = root.findtext("ExtraParams") or ""
    if "extra" in rules and rules["extra"] not in extra:
        errors.append(f"ExtraParams must contain {rules['extra']}")
    # Shutdown saves the index and database and is bounded at 10 s after a stalled client;
    # Docker's default stop timeout (10 s) would kill it first.
    if "--stop-timeout=60" not in extra.split():
        errors.append("ExtraParams must contain --stop-timeout=60")
    if "requires" in rules and rules["requires"] not in (root.findtext("Overview") or ""):
        errors.append(f"Overview must say it requires Lodestar {rules['requires']}+")
    # <Requires> is shown on the install page. Every template carries the element; one that
    # depends on something else must name it, or a user installs it and it can't start.
    requires_el = root.find("Requires")
    requires = (requires_el.text or "").strip() if requires_el is not None else ""
    if requires_el is None:
        errors.append("<Requires> element missing")
    if "requires" in rules and rules["requires"] not in requires:
        errors.append(f"<Requires> must name Lodestar {rules['requires']} or newer")
    for name in NETWORK_CONTAINER.findall(extra):
        if name not in requires:
            errors.append(f"<Requires> must name the running container {name!r} (ExtraParams has --network=container:{name})")
    # The first sentence says what the container is. A leading "Requires Lodestar v0.6+." comes first.
    overview = " ".join((root.findtext("Overview") or "").split())
    sentences = [x for x in re.split(r"(?<=[.!?])\s+", overview) if x and not x.startswith("Requires ")]
    first = sentences[0] if sentences else ""
    if overview and (len(first.split()) < 5 or not re.search(r"Lodestar|worker", first)):
        errors.append("Overview's first sentence must say what the container is (name Lodestar or the worker)")
    configs = root.findall("Config")
    for c in configs:
        for a in CONFIG_ATTRS:
            if c.get(a) is None:
                errors.append(f"Config {c.get('Name')!r} lacks {a}")
        if c.get("Type") not in {"Port", "Path", "Variable"}:
            errors.append(f"Config {c.get('Name')!r} has bad Type")
        # A field the app doesn't read yet is clutter that a user fills in for nothing. ("(planned)"
        # on the Peers (TCP) port is fine: that port is mapped for the peer listener.)
        if "not used yet" in (c.get("Description") or "").lower():
            errors.append(f"Config {c.get('Name')!r} is described as not used yet; remove the field")
    targets = {c.get("Target") for c in configs}
    for p in rules.get("ports", ()):
        if p not in targets:
            errors.append(f"port {p} not mapped")
    for p in rules.get("paths", ()):
        if p not in targets:
            errors.append(f"path {p} not mapped")
    for v in rules.get("vars", ()):
        if v not in targets:
            errors.append(f"variable {v} missing")
    data = next((c for c in configs if c.get("Target") == "/data"), None)
    if data is not None:
        default = data.get("Default") or ""
        if not default.startswith("/mnt/") or default.startswith("/mnt/user"):
            errors.append("/data must default to a pool path, not /mnt/user")
    if path.name == "lodestar.xml":
        udp = [c for c in configs if c.get("Target") == "6881" and c.get("Mode") == "udp"]
        tcp = [c for c in configs if c.get("Target") == "6881" and c.get("Mode") == "tcp"]
        if not udp or not tcp:
            errors.append("6881 needs both a udp and a tcp mapping")
    return errors


def unlisted(folder: Path) -> list[str]:
    """Templates in the folder that have no rules entry, so they would skip every check."""
    return [f"{p.name}: no rules entry in TEMPLATES; add one so this template is checked"
            for p in sorted(folder.glob("*.xml")) if p.name not in TEMPLATES]


def main() -> int:
    here = Path(__file__).parent
    failed = False
    for e in unlisted(here):
        print(e)
        failed = True
    for name, rules in TEMPLATES.items():
        errors = check(here / name, rules)
        for e in errors:
            print(f"{name}: {e}")
        print(f"{name}: {'FAIL' if errors else 'ok'}")
        failed |= bool(errors)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
