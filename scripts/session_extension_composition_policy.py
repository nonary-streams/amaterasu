"""Pure provider policy assembly for a future two-provider signed profile.

This does not authorize native effects or produce a runnable profile. The caller
must install the independently approved lifecycle graph, operation grants,
owner-control policy and selected schema documents before signing a package.
"""
from __future__ import annotations

import copy

import check_session_extension_composition as check
import prepare_session_extension_profile as legacy

ACTION_OWNER_OPERATIONS = {"describe_action_target", "inspect_action_effect",
                           "verify_action_effect", "check_action_effect"}


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
        participant = {"provider": provider, "contract_version": 2,
                       "depends_on": [] if provider == check.OWNER else [check.OWNER],
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


def build_bootstrap(base: dict, selected: list[dict], entries: dict, lifecycle_settings: dict,
                    enable_action: bool = False) -> tuple[dict, dict]:
    """Assemble a new bootstrap after external graph and API approval validation.

    The caller must validate the complete lifecycle settings, frozen method
    schemas, descriptor digests and component templates before using this result.
    Refuse existing extension policy instead of replacing an older installation.
    """
    bootstrap = copy.deepcopy(base)
    policy = bootstrap["policy"]
    principals = policy["principals"]
    settings = policy["settings"]
    providers = {item["descriptor"]["provider"]: item for item in selected}
    if set(providers) != {check.OWNER, check.DISPLAY}:
        raise ValueError("both providers are required")
    for identity in (legacy.LIFECYCLE, legacy.REGISTRY, legacy.ACTIONS, check.OWNER, check.DISPLAY):
        if identity in principals or identity in settings:
            raise ValueError("existing extension policy requires explicit migration")
    old_methods = policy.get("api_methods", [])
    def method_identity(entry: dict) -> tuple:
        method = entry["method"]
        return method["service"], method["method"], method["interface_major"]
    identities = [method_identity(item) for item in old_methods + entries["api_methods"]]
    if len(identities) != len(set(identities)):
        raise ValueError("provider API approval conflicts with existing policy")
    for identity, service in ((legacy.LIFECYCLE, legacy.LIFECYCLE_SERVICE),
                              (legacy.REGISTRY, legacy.REGISTRY_SERVICE),
                              (legacy.ACTIONS, legacy.ACTIONS_SERVICE)):
        principals[identity] = {"provide": [service], "call": []}
    for identity, item in providers.items():
        principals[identity] = copy.deepcopy(item["component"]["requested"])
    services = [check.OWNER + "/2", check.DISPLAY + "/2"]
    parent_chain = [legacy.NVHTTP, legacy.RTSP, legacy.STREAM, legacy.INPUT, legacy.MANAGEMENT]
    calls = {identity: [legacy.LIFECYCLE_SERVICE, legacy.REGISTRY_SERVICE, legacy.ACTIONS_SERVICE,
                        legacy.STATE, *services] for identity in parent_chain}
    calls.update({
        legacy.LIFECYCLE: ["amaterasu.launch/1", "amaterasu.input/1", legacy.LIFECYCLE_SERVICE,
                           legacy.REGISTRY_SERVICE, legacy.STATE, "amaterasu.display/1",
                           "amaterasu.virtual-display/1", *services],
        legacy.REGISTRY: [legacy.LIFECYCLE_SERVICE, legacy.STATE, *services],
        legacy.ACTIONS: [legacy.LIFECYCLE_SERVICE, legacy.REGISTRY_SERVICE, legacy.STATE, *services],
    })
    if enable_action:
        # The action service confirms that the triggering input delivery was
        # recorded before committing the reserved action.
        calls[legacy.ACTIONS].append("amaterasu.input/1")
        for identity in (legacy.ACTIONS, legacy.MANAGEMENT, legacy.INPUT, legacy.STREAM):
            calls[identity].append("amaterasu.virtual-display/1")
    for identity, requested in calls.items():
        principal = principals[identity]
        principal["call"] = sorted(set(principal.get("call", [])) | set(requested))
    principals[legacy.CONSOLE]["call"] = sorted(set(principals[legacy.CONSOLE].get("call", [])) |
        {legacy.REGISTRY_SERVICE, legacy.ACTIONS_SERVICE, legacy.LIFECYCLE_SERVICE, legacy.STATE, *services})
    policy["api_methods"] = old_methods + copy.deepcopy(entries["api_methods"])
    settings[legacy.REGISTRY] = {"descriptors": copy.deepcopy(entries["registry_descriptors"])}
    lifecycle = copy.deepcopy(lifecycle_settings)
    if any(name in lifecycle for name in ("participants", "input_actions")):
        raise ValueError("lifecycle settings must not override selected participants or actions")
    expected_actions = [check.OWNER + "/remove-owned"] if enable_action else []
    if entries["input_actions"] != expected_actions or bool(entries["action_grants"]) != enable_action:
        raise ValueError("action authorization does not match the profile selection")
    owner_operations = lifecycle.get("operation_grants", {}).get(check.OWNER, [])
    if enable_action and not ACTION_OWNER_OPERATIONS <= set(owner_operations):
        raise ValueError("owner action operation grants are incomplete")
    lifecycle["participants"] = copy.deepcopy(entries["participants"])
    lifecycle["input_actions"] = copy.deepcopy(entries["input_actions"])
    settings[legacy.LIFECYCLE] = lifecycle
    settings[legacy.ACTIONS] = {"input_service": legacy.INPUT, "grants": copy.deepcopy(entries["action_grants"])}
    launch = settings.setdefault("amaterasu.launch", {})
    observers = launch.setdefault("retirement_observers", [])
    if not isinstance(observers, list) or any(not isinstance(item, str) for item in observers):
        raise ValueError("invalid existing retirement observers")
    if legacy.LIFECYCLE not in observers:
        observers.append(legacy.LIFECYCLE)
    nvhttp = settings[legacy.NVHTTP]
    if "session_lifecycle_required" in nvhttp and nvhttp["session_lifecycle_required"] is not True:
        raise ValueError("existing lifecycle configuration conflicts with the profile")
    nvhttp["session_lifecycle_required"] = True
    capture = nvhttp["streaming"]["capture"]
    if "capture_provider" in capture and capture["capture_provider"] != "pipewire":
        raise ValueError("existing capture selection conflicts with PipeWire")
    capture["capture_provider"] = "pipewire"
    return bootstrap, calls


def lifecycle_graph(owner_approvals: list[dict], target_document: bytes, control_document: bytes,
                    method_names: dict[str, str]) -> dict:
    """Build the pinned resource-scoped owner graph from approved method names.

    Names are supplied only after the coordinator and owner's complete method
    contracts have been reviewed and matched. This does not validate routes,
    operation grants or runtime support and cannot by itself enable a profile.
    """
    import hashlib

    roles = {"control", "control_status", "settlement", "finalize", "finalize_status"}
    if set(method_names) != roles or any(not isinstance(name, str) or not name for name in method_names.values()):
        raise ValueError("complete owner method selections are required")
    for document, digest in ((target_document, check.TARGET_DIGEST), (control_document, check.CONTROL_DIGEST)):
        if hashlib.sha256(document).hexdigest() != digest:
            raise ValueError("composition schema bytes do not match the selected digest")
    names = {approval["method"]["method"] for approval in owner_approvals
             if approval.get("provider") == check.OWNER
             and approval.get("method", {}).get("service") == check.OWNER + "/2"
             and approval["method"].get("interface_major") == 2
             and approval["method"].get("interface_minor") == 0}
    if not set(method_names.values()) <= names:
        raise ValueError("owner method selection lacks an approval")
    target_schema = {"id": "amaterasu.display-target/1", "major": 1, "minor": 0, "digest": check.TARGET_DIGEST}
    control_schema = {"id": "amaterasu.display-control/1", "major": 1, "minor": 0, "digest": check.CONTROL_DIGEST}
    common = {"owner_provider": check.OWNER, "owner_service": check.OWNER + "/2",
              "output_capability": "amaterasu.display-target", "output_schema": target_schema,
              "domain_scope": "resource", "mutation_domains": []}
    control = {**copy.deepcopy(common), "dependent_provider": check.DISPLAY,
               "owner_method": method_ref(check.OWNER, method_names["control"]),
               "status_method": method_ref(check.OWNER, method_names["control_status"]),
               "settlement_method": method_ref(check.OWNER, method_names["settlement"]),
               "argument_schema": control_schema, "max_access": "exclusive"}
    finalize = {**copy.deepcopy(common), "method": method_ref(check.OWNER, method_names["finalize"]),
                "status_method": method_ref(check.OWNER, method_names["finalize_status"]),
                "capture_provider": "amaterasu.capture-pipewire/1", "selector_schema": "amaterasu.kwin-wl-output/1",
                "required_access": "exclusive"}
    return {"control_policies": [control], "finalize_policies": [finalize],
            "schema_documents": [{"reference": target_schema, "document": target_document.decode("utf-8")},
                                 {"reference": control_schema, "document": control_document.decode("utf-8")} ]}


def lifecycle_configuration(journal_path: str, graph: dict, additional_grants: dict[str, list[str]]) -> dict:
    """Combine the pinned graph with explicitly reviewed provider operations.

    The caller must confirm these operation names are implemented and necessary;
    this builder does not turn on composition or bypass the runtime gate.
    """
    if not isinstance(journal_path, str) or not journal_path.startswith("/"):
        raise ValueError("an absolute lifecycle journal path is required")
    if set(graph) != {"control_policies", "finalize_policies", "schema_documents"}:
        raise ValueError("complete installed graph is required")
    if set(additional_grants) != {check.OWNER, check.DISPLAY}:
        raise ValueError("reviewed operations for both providers are required")
    operations = copy.deepcopy(legacy.OPERATIONS)
    # The old combined provider's operations do not transfer to either v2
    # participant; the selected operations must be reviewed independently.
    operations.pop(legacy.PROVIDER, None)
    for provider, names in additional_grants.items():
        if not isinstance(names, list) or any(not isinstance(name, str) or not name or any(c not in "abcdefghijklmnopqrstuvwxyz0123456789_" for c in name) for name in names) or len(names) != len(set(names)):
            raise ValueError("invalid provider operation grants")
        operations[provider] = sorted(set(operations.get(provider, [])) | set(names))
    return {"journal_path": journal_path, "trusted_callers": sorted(operations),
            "session_delegates": [legacy.RTSP, legacy.STREAM],
            "action_service": legacy.ACTIONS, "settings_registry_caller": legacy.REGISTRY,
            "settings_registry_service": legacy.REGISTRY_SERVICE, "operation_grants": operations,
            "consumers": {"capture": [legacy.STREAM], "input": [legacy.STREAM, legacy.INPUT]},
            "consumer_requirements": {"execution_domain": "desktop",
                                      "capture_providers": ["amaterasu.capture-pipewire/1"],
                                      "selector_schemas": ["amaterasu.kwin-wl-output/1"]},
            **copy.deepcopy(graph)}
