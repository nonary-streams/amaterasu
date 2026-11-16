"""Apply reviewed grant changes to a local update's manifest and activation policy.

A component that starts publishing or subscribing to a topic needs that grant
in the bootstrap policy. The host only applies policy changes that the
installation's package activation policy approves, and only to the grants
each component requests in the signed manifest. This updates both from one
reviewed change file:

    {"amaterasu.rtsp": {"publish": {"add": ["topic/1"], "remove": []}}, ...,
     "/policy/settings/amaterasu.media-transport/extension_profiles": [...]}

A key that is a `/policy/settings/...` pointer sets that settings value; it
must name a principal the installed policy already configures. Principals
absent from the installed policy are skipped: the set does not contain them.
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
    settings = {path: value for path, value in changes.items() if path.startswith("/")}
    for path, value in sorted(settings.items()):
        set_setting(candidate, path, value)
    for principal, kinds in sorted(changes.items()):
        if principal in settings:
            continue
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
        if not path.startswith(("/policy/principals/", "/policy/settings/")):
            raise ValueError(f"changes may only touch principals and settings: {path}")
        if any(old != path and (old.startswith(path + "/") or path.startswith(old + "/"))
               for old in allowlist["changes"]):
            raise ValueError(f"overlapping prior approval: {path}")
        allowlist["changes"][path] = change
    if len(allowlist["changes"]) > 256:
        raise ValueError("policy change limit exceeded")
    return candidate, allowlist, delta, skipped


def set_setting(document, path, value):
    """Set one `/policy/settings/<principal>/...` value the policy configures."""
    keys = [key.replace("~1", "/").replace("~0", "~") for key in path.split("/")[1:]]
    if keys[:2] != ["policy", "settings"] or len(keys) < 4:
        raise ValueError(f"not a settings path: {path}")
    parent = document
    for key in keys[:-1]:
        if not isinstance(parent, dict) or key not in parent:
            raise ValueError(f"settings path is not configured: {path}")
        parent = parent[key]
    parent[keys[-1]] = value
