#!/usr/bin/env python3
"""Prepare a local, opt-in signed Linux set with a selected display extension.

The root and package must be new paths. This does not stage, activate, restart,
install, mutate a display, or configure the firewall. Native plugins run as
trusted code. End-to-end session/display behavior requires separate validation.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import tempfile

LIFECYCLE = "amaterasu.session-lifecycle"
REGISTRY = "amaterasu.extension-registry"
ACTIONS = "amaterasu.extension-actions"
PROVIDER = "nonary.display-settings"
PROVIDER_SERVICE = PROVIDER + "/1"
LIFECYCLE_SERVICE = LIFECYCLE + "/1"
REGISTRY_SERVICE = REGISTRY + "/1"
ACTIONS_SERVICE = ACTIONS + "/1"
STATE = "amaterasu.state/1"
MANAGEMENT = "amaterasu.management"
INPUT = "amaterasu.input"
NVHTTP = "amaterasu.nvhttp"
RTSP = "amaterasu.rtsp"
STREAM = "amaterasu.stream-session"
CONSOLE = "amaterasu.console"

# These are operation grants, not a native-code sandbox or user role grants.
OPERATIONS = {
    NVHTTP: ["prepare", "bind_application", "application_retired", "end_session", "lookup", "snapshot", "recover_session"],
    RTSP: ["prepare_stream", "resume_stream", "lookup", "snapshot"],
    STREAM: ["validate_candidate", "ack_binding", "start_stream", "pause_stream", "retire_stream", "binding_validate", "capture_invalidated", "resource_invalidated", "issue_input_binding", "lookup"],
    INPUT: ["validate_input_binding", "reserve_input_action", "finish_input_action", "lookup_input_action"],
    ACTIONS: ["authorize_action", "consume_action_permit", "begin_action_transition", "complete_action", "consume_input_reservation", "cancel_input_reservation", "lookup"],
    REGISTRY: ["resolve_settings_scope"],
    PROVIDER: ["describe_action_target", "refresh_provider_observations", "verify_provider_epoch", "verify_action_effect", "check_action_effect"],
}


def run(*arguments: object) -> str:
    result = subprocess.run([str(argument) for argument in arguments], text=True, capture_output=True, timeout=120)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    return result.stdout.strip()


def method_ref(name: str) -> dict:
    return {"service": PROVIDER_SERVICE, "method": name, "interface_major": 1,
            "interface_minor": 0, "schema_revision": 1}


def prepare(bootstrap: dict, root: Path, descriptor: dict, descriptor_digest: str, approvals: list[dict], component: dict) -> dict:
    if descriptor.get("provider") != PROVIDER or component.get("id") != PROVIDER:
        raise ValueError("unexpected display provider identity")
    if any(approval.get("provider") != PROVIDER or approval.get("method", {}).get("service") != PROVIDER_SERVICE for approval in approvals):
        raise ValueError("provider API approvals do not match the selected provider")
    expected = {"settings_validate", "resource_plan", "lifecycle_callback", "lifecycle_status", "binding_validate"}
    if len(approvals) != len(expected) or {item["method"]["method"] for item in approvals} != expected:
        raise ValueError("provider API approvals are incomplete or duplicated")
    policy = bootstrap["policy"]
    principals = policy["principals"]
    settings = policy["settings"]
    for identity, service in ((LIFECYCLE, LIFECYCLE_SERVICE), (REGISTRY, REGISTRY_SERVICE), (ACTIONS, ACTIONS_SERVICE)):
        principals[identity] = {"provide": [service], "call": []}
    principals[PROVIDER] = component["requested"]
    parent_chain = [NVHTTP, RTSP, STREAM, INPUT, MANAGEMENT]
    calls = {
        identity: [LIFECYCLE_SERVICE, REGISTRY_SERVICE, ACTIONS_SERVICE, STATE, PROVIDER_SERVICE]
        for identity in parent_chain
    }
    calls.update({
        LIFECYCLE: ["amaterasu.launch/1", "amaterasu.input/1", REGISTRY_SERVICE, PROVIDER_SERVICE,
                    STATE, "amaterasu.display/1", "amaterasu.virtual-display/1"],
        REGISTRY: [LIFECYCLE_SERVICE, STATE, PROVIDER_SERVICE],
        ACTIONS: [LIFECYCLE_SERVICE, REGISTRY_SERVICE, STATE, PROVIDER_SERVICE],
    })
    for identity, services in calls.items():
        principal = principals[identity]
        principal["call"] = sorted(set(principal.get("call", [])) | set(services))
    principals[CONSOLE]["call"] = sorted(set(principals[CONSOLE].get("call", [])) |
                                         {REGISTRY_SERVICE, ACTIONS_SERVICE, LIFECYCLE_SERVICE, STATE, PROVIDER_SERVICE})
    validation_callers = [REGISTRY, LIFECYCLE, MANAGEMENT, INPUT, STREAM, RTSP, NVHTTP, CONSOLE]
    for approval in approvals:
        if approval["method"]["method"] == "settings_validate":
            approval["callers"] = validation_callers
    installed = policy.get("api_methods", [])
    def method_identity(approval: dict) -> tuple:
        method = approval["method"]
        return (method["service"], method["method"], method["interface_major"])
    identities = [method_identity(approval) for approval in installed + approvals]
    if len(identities) != len(set(identities)):
        raise ValueError("provider API approval conflicts with existing policy")
    policy["api_methods"] = installed + approvals
    grants = [{"caller": identity, "scope": {"kind": "host"}, "read": True,
               "write": identity in (MANAGEMENT, CONSOLE)} for identity in validation_callers if identity != REGISTRY]
    settings[REGISTRY] = {"descriptors": [{"descriptor": descriptor,
        "descriptor_sha256": descriptor_digest,
        "approved_methods": [item["method"] for item in approvals], "grants": grants, "migrations": []}]}
    defaults = {option["id"]: option["definition"]["default"] for option in descriptor["options"]}
    if defaults.get("enabled") is not False:
        raise ValueError("display extension must start disabled")
    journal = root / "data" / "session-lifecycle"
    journal.mkdir(mode=0o700)
    settings[LIFECYCLE] = {
        "journal_path": str(journal / "journal.json"),
        "trusted_callers": sorted(OPERATIONS), "session_delegates": [RTSP, STREAM],
        "input_actions": [], "action_service": ACTIONS, "settings_registry_caller": REGISTRY,
        "settings_registry_service": REGISTRY_SERVICE,
        "operation_grants": OPERATIONS,
        "consumers": {"capture": [STREAM], "input": [STREAM, INPUT]},
        "consumer_requirements": {"execution_domain": "desktop", "capture_providers": ["amaterasu.capture-pipewire/1"],
                                  "selector_schemas": ["amaterasu.kwin-wl-output/1"]},
        "participants": [{"provider": PROVIDER, "plan_method": method_ref("resource_plan"),
            "callback_method": method_ref("lifecycle_callback"), "status_method": method_ref("lifecycle_status"),
            "binding_validate_method": method_ref("binding_validate"), "settings_revision": "bootstrap-v1",
            "settings": defaults, "capabilities": [], "cleanup_methods": []}],
    }
    settings[ACTIONS] = {"input_service": INPUT, "grants": []}
    settings.setdefault("amaterasu.launch", {})["retirement_observers"] = [LIFECYCLE]
    settings[NVHTTP]["session_lifecycle_required"] = True
    # The implemented binding adapter supports PipeWire/KWin SDR only.
    settings[NVHTTP]["streaming"]["capture"]["capture_provider"] = "pipewire"
    return calls


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("publisher", type=Path)
    parser.add_argument("root", type=Path)
    parser.add_argument("artifacts", type=Path)
    parser.add_argument("package", type=Path)
    parser.add_argument("sequence", type=int)
    parser.add_argument("provider_repo", type=Path)
    parser.add_argument("provider_library", type=Path)
    parser.add_argument("linux_options", nargs=argparse.REMAINDER, help="options passed to init-linux-session after --")
    args = parser.parse_args()
    if args.root.exists() or args.package.exists():
        parser.error("root and package must be new paths")
    examples = args.provider_repo / "examples"
    descriptor = json.loads((examples / "settings-descriptor.json").read_text())
    approvals = json.loads((examples / "lifecycle-api-approvals.json").read_text())["api_methods"]
    template = examples / "component-linux.json"
    component = json.loads(template.read_text())
    options = args.linux_options[1:] if args.linux_options[:1] == ["--"] else args.linux_options
    if any(option == "--capture-output" or option.startswith("--capture-output=")
           or option == "--enable-virtual-screen-capture" for option in options):
        parser.error("session-extension capture currently requires PipeWire; KMS output and virtual-screen capture selection are unsupported")
    run(args.publisher, "init-linux-session", args.root, *options)
    bootstrap_path = args.root / "bootstrap.json"
    bootstrap = json.loads(bootstrap_path.read_text())
    descriptor_digest = run(args.publisher, "extension-descriptor-digest", examples / "settings-descriptor.json")
    calls = prepare(bootstrap, args.root.resolve(), descriptor, descriptor_digest, approvals, component)
    bootstrap_path.write_text(json.dumps(bootstrap, indent=2) + "\n")
    with tempfile.TemporaryDirectory(prefix="amaterasu-extension-grants-") as temporary:
        call_path = Path(temporary) / "calls.json"
        call_path.write_text(json.dumps(calls))
        run(args.publisher, "pack-linux-session-extensions", args.root, args.artifacts, args.package,
            args.sequence, "--external-component", template, args.provider_library, "--call-grants", call_path)
        run(args.publisher, "verify", args.root, args.package)
    print("Prepared and verified the opt-in signed set; it has not been staged or activated.")
    print("Provider effects start disabled; lifecycle, platform and live streaming require qualification.")


if __name__ == "__main__":
    main()
