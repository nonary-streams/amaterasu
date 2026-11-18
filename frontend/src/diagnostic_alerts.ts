namespace Amaterasu {
  type AlertFields = Record<string, unknown>;
  type LifecycleAlert = { key: string; message: string; source: string; operation: string; title: string; at: number; occurrences: number };

  function fields(value: unknown): AlertFields {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid stream issue record.");
    return value as AlertFields;
  }

  const issueMessages: Record<string, string> = {
    optional_client_features: "Ignored unsupported optional client features.",
    intra_refresh_unavailable: "The encoder does not support the requested intra-refresh mode.",
    bandwidth_adjusted: "Reduced the video bitrate to allow room for audio and network overhead.",
    refresh_hint_ignored: "Ignored an inconsistent display refresh hint.",
    hevc_capability_hint_ignored: "Ignored an unrecognized HEVC capability hint.",
    invalid_optional_hints: "Ignored invalid optional client hints.",
    compatibility_defaults: "Used defaults for optional settings omitted by the client.",
    audio_quality_default: "Used the configured audio quality.",
    proton_policy_unavailable: "The global Proton controller preference was unavailable.",
    proton_hook_unavailable: "Could not confirm the Proton controller helper.",
    application_output_unavailable: "Could not use the configured application output.",
    detached_command_unavailable: "Could not start an optional detached command.",
    capture_fallback: "Switched to a compatible capture source.",
    encoder_fallback: "Switched to a compatible encoder.",
  };

  function issueText(issue: AlertFields): string {
    if (typeof issue.code !== "string" || !issue.code) throw new Error("Stream issue has no code.");
    const words = (value: string) => value.replaceAll("_", " ");
    const parts = [Object.hasOwn(issueMessages, issue.code) ? issueMessages[issue.code] : words(issue.code) + "."];
    if (typeof issue.reason === "string" && issue.reason && issue.reason !== issue.code) parts.push("Reason: " + words(issue.reason) + ".");
    if (typeof issue.fallback === "string") parts.push("Used " + words(issue.fallback) + ".");
    for (const [name, label] of [["requested", "Requested"], ["effective", "Effective"], ["os_error", "OS error"]] as const) {
      const value = issue[name];
      if (value == null) continue;
      if (typeof value !== "boolean" && typeof value !== "string" && !Number.isSafeInteger(value)) throw new Error("Invalid stream issue value.");
      let display = typeof value === "string" ? words(value) : String(value);
      if (typeof value === "number" && name !== "os_error") {
        if (issue.code === "bandwidth_adjusted") display = value + " kbps";
        else if (issue.code === "refresh_hint_ignored") display = value / 100 + " Hz";
        else if (issue.code === "optional_client_features" || issue.code === "invalid_optional_hints") display = "0x" + value.toString(16).toUpperCase();
      }
      parts.push(label + ": " + display + ".");
    }
    return parts.join(" ");
  }

  export function retainedStreamWarnings(detail: HistoryDetailReply["detail"]): string {
    if (!detail) return "Warning history unavailable.";
    const messages = new Set<string>();
    for (const event of detail.events) {
      const field = event.event_type === "stream_warnings" ? "warnings" : event.event_type === "stream_adjustments" ? "adjustments" : null;
      if (!field) continue;
      const items = fields(event.payload)[field];
      if (!Array.isArray(items)) throw new Error("Invalid stream warning history.");
      for (const item of items) messages.add(issueText(fields(item)));
    }
    if (detail.events_truncated) messages.add("Some earlier events are unavailable.");
    return [...messages].join(" ") || "No warnings recorded.";
  }

  export function lifecycleAlerts(value: unknown): { alerts: LifecycleAlert[]; incomplete: boolean } {
    const reply = fields(value);
    if (reply.ok !== true || typeof reply.incomplete !== "boolean" || !Array.isArray(reply.issues))
      throw new Error("Stream issue history is unavailable.");
    const alerts = new Map<string, LifecycleAlert>();
    for (const value of reply.issues) {
      const entry = fields(value), issue = fields(entry.issue);
      if (typeof issue.operation !== "string" || typeof entry.publisher !== "string" || !Number.isSafeInteger(entry.at_ms) ||
          !["critical_failure", "advisory_warning", "demoted_warning"].includes(String(issue.disposition)))
        throw new Error("Invalid stream issue identity.");
      const key = JSON.stringify([issue.operation, entry.publisher, issue.layer, issue.phase, issue.disposition,
        issue.code, issue.reason, issue.caused_by, issue.fallback, issue.requested, issue.effective, issue.os_error]);
      const previous = alerts.get(key);
      alerts.set(key, { key, occurrences: (previous?.occurrences ?? 0) + 1, message: issueText(issue), source: entry.publisher, operation: issue.operation, title: issue.disposition === "critical_failure" ? "Failure" : "Warning", at: Math.max(previous?.at ?? 0, Number(entry.at_ms)) });
    }
    return { alerts: [...alerts.values()].sort((a, b) => b.at - a.at), incomplete: reply.incomplete };
  }

  export function renderDiagnosticAlerts(context: PageContext, target: HTMLElement): () => Promise<void> {
    const storageKey = "amaterasu.lifecycle-dismissals." + context.api.hostOrigin;
    const dismissed = new Set<string>();
    try {
      const saved: unknown = JSON.parse(window.localStorage.getItem(storageKey) ?? "[]");
      if (Array.isArray(saved)) for (const key of saved.slice(-64)) if (typeof key === "string") dismissed.add(key);
    } catch { /* Dismissals still work for this page. */ }
    let snapshot: ReturnType<typeof lifecycleAlerts> | undefined;
    let status = "Reading stream issues…";
    let generation = 0;
    let timer: number | undefined;
    function save(): void {
      while (dismissed.size > 64) dismissed.delete(dismissed.values().next().value!);
      try { window.localStorage.setItem(storageKey, JSON.stringify([...dismissed])); } catch { /* Volatile dismissal. */ }
      render();
    }
    function render(): void {
      const list = el("div");
      list.setAttribute("aria-live", "polite");
      for (const item of snapshot?.alerts ?? []) {
        if (dismissed.has(item.key)) continue;
        list.append(el("article", "notice", el("h3", "", item.title), el("p", "", item.message),
          el("p", "muted", item.source + " · " + item.operation + " · " + new Date(item.at).toLocaleString() + (item.occurrences > 1 ? " · " + item.occurrences + " occurrences" : "")),
          button("Dismiss", () => { dismissed.add(item.key); save(); })));
      }
      if (status) list.append(el("p", "muted", status));
      else if (!list.childElementCount) list.append(el("p", "muted", "No undismissed stream issues."));
      if (snapshot?.incomplete) list.append(el("p", "notice", "Some stream issue records are unavailable."));
      const restore = button("Restore dismissed alerts", () => { dismissed.clear(); save(); });
      restore.disabled = !dismissed.size;
      target.replaceChildren(panel("Alerts", list, restore));
    }
    async function refresh(): Promise<void> {
      const current = ++generation;
      window.clearTimeout(timer);
      if (!context.api.allows("session.read")) {
        snapshot = undefined;
        status = "Session read access is required to view stream issues.";
        render();
        return;
      }
      try {
        const result = lifecycleAlerts(await context.api.post<unknown>("nvhttp/launch_status", { issues_only: true }));
        if (current !== generation || !target.isConnected) return;
        snapshot = result;
        status = "";
      } catch {
        if (current !== generation || !target.isConnected) return;
        status = "Stream issues could not be refreshed.";
      }
      render();
      timer = window.setTimeout(() => { if (target.isConnected) void refresh(); }, 5000);
    }
    render();
    return refresh;
  }
}
