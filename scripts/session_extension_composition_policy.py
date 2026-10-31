"""Pure provider policy assembly for a future two-provider signed profile.

This does not authorize native effects or produce a runnable profile. The caller
must install the independently approved lifecycle graph, operation grants,
owner-control policy and selected schema documents before signing a package.
"""
from __future__ import annotations

import copy

import check_session_extension_composition as check
import prepare_session_extension_profile as legacy


def method_ref(provider: str, name: str) -> dict:
    return {"service": provider + "/2", "method": name, "interface_major": 2,
            "interface_minor": 0, "schema_revision": 1}


def approved_action(descriptor: dict, enabled: bool) -> str:
    action_id = check.OWNER + "/remove-owned"
    actions = descriptor.get("actions")
    if not isinstance(actions, list):
        raise ValueError("invalid owner actions")
    if not actions:
        if enabled:
            raise ValueError("owner does not advertise remove-owned")
        return action_id
    if len(actions) != 1:
        raise ValueError("unsupported owner actions")
    action = actions[0]
    if (not isinstance(action, dict) or action.get("id") != action_id
            or action.get("execution") != "session_mutation"
            or set(action.get("allowed_targets", [])) != {"session", "stream"}
            or action.get("arguments") != {}
            or action.get("plan") != method_ref(check.OWNER, "action_plan")
            or action.get("execute") != method_ref(check.OWNER, "action_execute")
            or action.get("status") != method_ref(check.OWNER, "action_status")):
        raise ValueError("unsupported owner action contract")
    return action_id


def provider_entries(selected: list[dict], enable_action: bool = False) -> dict:
    """Build registry entries, approvals and ordered participants from two selections.

    Each selection has descriptor, descriptor_digest, component, and approvals.
    The caller must compute and verify each typed descriptor digest against the
    selected bytes and independently validate complete method schemas first.
    Caller policy is never modified. This helper does not grant broker calls or
    install the owner graph.
    """
    by_id = {}
    for item in selected:
        if not isinstance(item, dict) or set(item) != {"descriptor", "descriptor_digest", "component", "approvals"}:
            raise ValueError("invalid provider selection")
        descriptor = item["descriptor"]
        if not isinstance(descriptor, dict):
            raise ValueError("invalid provider descriptor")
        identity = descriptor.get("provider")
        if identity in by_id:
            raise ValueError("duplicate provider selection")
        by_id[identity] = item
    if set(by_id) != {check.DISPLAY, check.OWNER}:
        raise ValueError("both selected providers are required")
    validation_callers = [legacy.REGISTRY, legacy.ACTIONS, legacy.LIFECYCLE, legacy.MANAGEMENT,
                          legacy.INPUT, legacy.STREAM, legacy.RTSP, legacy.NVHTTP, legacy.CONSOLE]
    registry = []
    approvals = []
    participants = []
    for provider, revision, methods in ((check.OWNER, 1, check.OWNER_METHODS),
                                         (check.DISPLAY, 2, check.DISPLAY_METHODS)):
        item = by_id[provider]
        descriptor = item["descriptor"]
        check.check_provider(descriptor, item["component"], item["approvals"], provider, revision, methods)
        digest = item["descriptor_digest"]
        if not isinstance(digest, str) or len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
            raise ValueError("invalid descriptor digest")
        selected_approvals = copy.deepcopy(item["approvals"])
        for approval in selected_approvals:
            name = approval["method"]["method"]
            if name == "settings_validate":
                approval["callers"] = validation_callers.copy()
            elif name in {"action_plan", "action_execute", "action_status"}:
                approval["callers"] = [legacy.REGISTRY, legacy.ACTIONS, legacy.MANAGEMENT, legacy.INPUT, legacy.STREAM]
            else:
                approval["callers"] = [legacy.LIFECYCLE]
        grants = [{"caller": caller, "scope": {"kind": "host"}, "read": True,
                   "write": caller in (legacy.MANAGEMENT, legacy.CONSOLE)}
                  for caller in validation_callers if caller != legacy.REGISTRY]
        registry.append({"descriptor": copy.deepcopy(descriptor), "descriptor_sha256": digest,
                         "approved_methods": [entry["method"] for entry in selected_approvals],
                         "grants": grants, "migrations": []})
        defaults = {option["id"]: option["definition"]["default"] for option in descriptor["options"]}
        participant = {"provider": provider, "depends_on": [] if provider == check.OWNER else [check.OWNER],
                       "plan_method": method_ref(provider, "resource_plan"),
                       "callback_method": method_ref(provider, "lifecycle_callback"),
                       "status_method": method_ref(provider, "lifecycle_status"),
                       "settings_revision": "bootstrap-v1", "settings": defaults,
                       "capabilities": [], "cleanup_methods": []}
        if provider == check.OWNER:
            participant["binding_validate_method"] = method_ref(provider, "binding_validate")
        participants.append(participant)
        approvals.extend(selected_approvals)
    if by_id[check.DISPLAY]["descriptor"].get("actions"):
        raise ValueError("display settings actions are unsupported")
    action_id = approved_action(by_id[check.OWNER]["descriptor"], enable_action)
    action_grants = [{"principal": identity, "action_ids": [action_id],
                      "target_kinds": ["session", "stream"], "execution_classes": ["session_mutation"]}
                     for identity in (legacy.MANAGEMENT, legacy.INPUT, legacy.STREAM)] if enable_action else []
    return {"registry_descriptors": registry, "api_methods": approvals,
            "participants": participants, "input_actions": [action_id] if enable_action else [],
            "action_grants": action_grants}
