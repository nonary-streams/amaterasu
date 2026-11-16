"""Apply reviewed grant changes to a local update's manifest and activation policy.

A component that starts publishing or subscribing to a topic needs that grant
in the bootstrap policy. The host only applies policy changes that the
installation's package activation policy approves, and only to the grants
each component requests in the signed manifest. This updates both from one
reviewed change file:

    {"amaterasu.rtsp": {"publish": {"add": ["topic/1"], "remove": []}}, ...}

Principals absent from the installed policy are skipped: the set does not
contain them.
"""
from copy import deepcopy

from local_bound_input import differences, policy_value

GRANT_KINDS = {"publish", "subscribe", "call", "provide", "native_use", "native_provide", "hook"}


def apply_grant_changes(bootstrap, manifest, approved, changes):
    """Return (candidate bootstrap, approved policy, delta, skipped principals)."""
    candidate = deepcopy(bootstrap)
    principals = candidate["policy"]["principals"]
    components = {component["id"]: component for component in manifest["components"]}
    skipped = []
    for principal, kinds in sorted(changes.items()):
        if principal not in principals or principal not in components:
            skipped.append(principal)
            continue
        for kind, change in kinds.items():
            if kind not in GRANT_KINDS:
                raise ValueError(f"unknown grant kind: {kind}")
            add, remove = set(change.get("add", [])), set(change.get("remove", []))
            for grants in (principals[principal], components[principal]["requested"]):
                grants[kind] = sorted((set(grants.get(kind, [])) - remove) | add)
    allowlist = deepcopy(approved)
    for path, change in approved["changes"].items():
        if policy_value(bootstrap, path) != change["after"]:
            raise ValueError(f"existing approval is not applied: {path}")
    delta = differences(bootstrap, candidate)
    for path, change in delta.items():
        if not path.startswith("/policy/principals/"):
            raise ValueError(f"grant changes may only touch principals: {path}")
        if any(old != path and (old.startswith(path + "/") or path.startswith(old + "/"))
               for old in allowlist["changes"]):
            raise ValueError(f"overlapping prior approval: {path}")
        allowlist["changes"][path] = change
    if len(allowlist["changes"]) > 256:
        raise ValueError("policy change limit exceeded")
    return candidate, allowlist, delta, skipped
