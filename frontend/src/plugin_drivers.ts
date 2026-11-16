namespace Amaterasu {
  export type DriverSetupPhase = "checking" | "awaiting_approval" | "waiting_for_idle" | "installing" |
    "reboot_required" | "manual_action_required" | "ready_to_activate" | "activating" | "ready" | "failed" | "cancelled";
  export type DriverReadiness = "checking" | "missing" | "incompatible" | "access_denied" | "unsupported" |
    "installing" | "ready" | "reboot_required" | "manual_action_required" | "failed";
  export interface PluginDriver {
    id: string; name: string; publisher: string; reason: string; requiredBy: string[]; requiredComponents: string[];
    state: DriverReadiness; message: string; mayRequireReboot: boolean; installedVersion: string | null;
    offeredVersion: string | null; update: "unknown" | "available" | "current" | "newer_installed";
  }
  export interface PluginSetupOperation {
    id: string; revision: string; installationRevision: string; phase: DriverSetupPhase;
    approval: string; drivers: PluginDriver[]; error: string | null; cancellable: boolean;
  }
  export interface PluginDriverStatus {
    supported: boolean; installationRevision: string | null;
    drivers: PluginDriver[]; operation: PluginSetupOperation | null;
  }
  const DRIVER_STATES: readonly string[] = ["checking", "missing", "incompatible", "access_denied", "unsupported", "installing", "ready", "reboot_required", "manual_action_required", "failed"];
  const DRIVER_PHASES: readonly string[] = ["checking", "awaiting_approval", "waiting_for_idle", "installing", "reboot_required", "manual_action_required", "ready_to_activate", "activating", "ready", "failed", "cancelled"];
  const DRIVER_DIGEST = /^[a-f0-9]{64}$/;
  function driverRecord(value: unknown): Record<string, unknown> {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Driver status is invalid.");
    return value as Record<string, unknown>;
  }
  function driverText(value: unknown, maximum = 4096): string {
    if (typeof value !== "string" || value.length > maximum || /[\u0000-\u001f\u007f]/u.test(value)) throw new Error("Driver status is invalid.");
    return value;
  }
  function driverDigest(value: unknown): string {
    if (typeof value !== "string" || !DRIVER_DIGEST.test(value)) throw new Error("Driver identity is invalid.");
    return value;
  }
  function driverRows(value: unknown): PluginDriver[] {
    if (!Array.isArray(value) || value.length > 64) throw new Error("Driver inventory is invalid.");
    const ids = new Set<string>();
    return value.map((entry: unknown) => {
      const row = driverRecord(entry);
      const id = driverText(row.id, 128);
      if (!/^[a-z0-9._-]{1,128}$/.test(id) || ids.has(id) || !Array.isArray(row.required_by) || row.required_by.length > 128 ||
          !DRIVER_STATES.includes(String(row.state)) || typeof row.may_require_reboot !== "boolean") throw new Error("Driver inventory is invalid.");
      ids.add(id);
      const components = row.required_components ?? [];
      if (!Array.isArray(components) || components.length > 1024) throw new Error("Driver inventory is invalid.");
      const update = row.update ?? "unknown";
      if (!["unknown", "available", "current", "newer_installed"].includes(String(update))) throw new Error("Driver update status is invalid.");
      const installedVersion = row.installed_version === null ? null : driverText(row.installed_version,128);
      const offeredVersion = row.offered_version == null ? null : driverText(row.offered_version,128);
      if (update !== "unknown" && (!installedVersion || !offeredVersion)) throw new Error("Driver update versions are missing.");
      return { id, name:driverText(row.name,128), publisher:driverText(row.publisher,128), reason:driverText(row.reason,1024),
        requiredBy:row.required_by.map((id:unknown)=>driverText(id,128)), requiredComponents:components.map((id:unknown)=>driverText(id,128)), state:row.state as DriverReadiness,
        message:driverText(row.message), mayRequireReboot:row.may_require_reboot,
        installedVersion, offeredVersion, update:update as PluginDriver["update"] };
    });
  }
  export function parsePluginDriverStatus(value: unknown): PluginDriverStatus {
    const reply = driverRecord(value);
    if (reply.ok !== true || typeof reply.supported !== "boolean") throw new Error("Driver setup is unavailable.");
    if (!reply.supported) return {supported:false,installationRevision:null,drivers:[],operation:null};
    const installationRevision = driverDigest(reply.installation_revision);
    let operation: PluginSetupOperation | null = null;
    if (reply.operation !== null) {
      const row = driverRecord(reply.operation);
      const revision = driverText(row.revision,20);
      if (!/^[1-9][0-9]{0,19}$/.test(revision) || BigInt(revision) > 18446744073709551615n ||
          !DRIVER_PHASES.includes(String(row.phase)) || typeof row.cancellable !== "boolean") throw new Error("Plugin setup operation is invalid.");
      operation = {id:driverText(row.id,128),revision,installationRevision:driverDigest(row.installation_revision),phase:row.phase as DriverSetupPhase,
        approval:driverDigest(row.approval),drivers:driverRows(row.drivers),error:row.error === null ? null : driverText(row.error),cancellable:row.cancellable};
      if (operation.installationRevision !== installationRevision) throw new Error("Plugin setup changed. Refresh to review it again.");
    }
    return {supported:true,installationRevision,drivers:driverRows(reply.drivers),operation};
  }

  export function driverSetupError(code: string): string {
    const known: Record<string,string> = {
      driver_service_not_provisioned:"An administrator needs to enable driver installation on this computer.",
      driver_publisher_not_approved:"This driver's publisher needs approval on this computer.",
      plugin_revision_conflict:"This installation changed. Review the plugin again before continuing.",
      driver_stale_approval:"The required drivers changed. Review setup again before approving installation.",
      plugin_trial_failed:"The plugin could not start. The previous working modules have been restored.",
      driver_not_cancellable:"Installation has reached a step that must finish before it can stop.",
    };
    if (known[code]) return known[code];
    const conflict = /^plugin_component_conflict:(.+)$/.exec(code);
    if (conflict) return `This plugin contains ${conflict[1]}, which is already installed. Adding a plugin cannot replace it.`;
    const dependency = /^plugin_dependency_conflict:([^:]+):(.+)$/.exec(code);
    if (dependency) return `${dependency[1]} needs a different version of ${dependency[2]}.`;
    return code;
  }

  export function driverUpdateLabel(update: PluginDriver["update"]): string {
    return ({unknown:"Update availability unknown",available:"Update available in this plugin",current:"Matches this plugin’s driver version",newer_installed:"Newer version installed"})[update];
  }
  export function driverStateLabel(state: DriverReadiness): string {
    return ({checking:"Checking",missing:"Not installed",incompatible:"Different version required",access_denied:"Device access required",unsupported:"Unsupported system",installing:"Installing",ready:"Ready",reboot_required:"Restart computer required",manual_action_required:"Action required",failed:"Setup failed"})[state];
  }
  export function driverSetupMessage(phase: DriverSetupPhase): string {
    return ({checking:"Checking required drivers…",awaiting_approval:"This plugin needs drivers before it can be used.",waiting_for_idle:"Setup will continue after the current stream ends.",installing:"Installing required drivers…",reboot_required:"Restart this computer when convenient, then check setup again.",manual_action_required:"Complete the action shown below, then check setup again.",ready_to_activate:"Drivers are ready. Finish adding the plugin when the host is idle.",activating:"Activating the plugin…",ready:"The plugin is ready.",failed:"Plugin setup needs attention.",cancelled:"Plugin setup was cancelled."})[phase];
  }
}
