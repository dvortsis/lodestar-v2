"""Record exact Terraform-created IDs for the independently installed broker."""
import importlib.util
from pathlib import Path

_path = Path(__file__).resolve().parents[1] / "aws/fleet-control/cleanup_handler.py"
_spec = importlib.util.spec_from_file_location("fleet_cleanup_contract", _path)
contract = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(contract)


def authorization(fleet, hosts, linode, account, now):
    linode.check_account(account)
    nodes = {str(n["id"]): n for n in linode.linodes()}
    walls = {str(w["id"]): w for w in linode.firewalls()}
    doc = {"version": 1, "account_euuid": account, "region": "us-east", "deployment_id": "fleet",
           "created_at": fleet["created_at"], "expires_at": fleet["expires_at"], "linodes": [], "firewalls": []}
    bindings = {}
    for role in fleet["roles"]:
        host = hosts["main" if role == "main" else "worker-01"]
        node, wall = nodes[str(host["linode_id"])], walls[str(host["firewall_id"])]
        if host["address"] not in node.get("ipv4", []):
            raise contract.Refused("Terraform address/ID mismatch")
        expected = {k: node[k] for k in ("id", "label", "type", "created")}
        expected["role"] = role
        contract.check_node(node, expected, contract.timestamp(fleet["created_at"]))
        doc["linodes"].append(expected)
        bindings.setdefault(wall["id"], {k: wall[k] for k in ("id", "label", "created")})
        bindings[wall["id"]].setdefault("linode_ids", []).append(node["id"])
        bindings[wall["id"]]["admin_ipv4"] = host["admin_ipv4"]
    doc["firewalls"] = list(bindings.values())
    contract.validate_authorization(doc, account, contract.timestamp(fleet["created_at"]), now)
    for wall in doc["firewalls"]:
        if not contract.TAGS <= set(walls[str(wall["id"])].get("tags") or []):
            raise contract.Refused("firewall ownership tags incomplete")
        for device in linode.firewall_devices(wall["id"]):
            entity = device.get("entity", {})
            if entity.get("type") != "linode" or entity.get("id") not in wall["linode_ids"]:
                raise contract.Refused("firewall has an unapproved member")
    return doc
