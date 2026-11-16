namespace Amaterasu {
  export interface StreamCompatibilityAlert {
    key: string;
    featureFlags: string;
    occurrences: number;
    lastSeen: string;
    adjustment?: {stream: string; code: string; requested: number; effective: number; reason?: string};
    warning?: {stream: string; code: string; source: string; reason: string; os_error?: number; requested?: boolean | number | string; effective?: boolean | number | string};
  }

  // Match only this host-produced warning and its authenticated module target,
  // never arbitrary warning prose or caller-scoped diagnostic messages.
  export function streamCompatibilityAlerts(contents: string): StreamCompatibilityAlert[] {
    const alerts = new Map<string, StreamCompatibilityAlert>();
    const pattern = /^\[([^\]\r\n]{1,64})\]: Warning: amaterasu\.rtsp::rtsp\.negotiation: Optional client features (0x[0-9a-f]{8}) are unsupported and ignored; streaming can continue\.$/;
    for (const line of contents.split("\n")) {
      const warningLine = /^\[([^\]\r\n]{1,64})\]: Warning: amaterasu\.nvhttp::stream\.warning: (\{.{1,2048}\})$/.exec(line.replace(/\r$/, ""));
      if (warningLine) {
        try {
          const value = JSON.parse(warningLine[2]!);
          const warning = value.warning;
          const scalar = (item: unknown) => item === undefined || typeof item === "boolean" ||
            (typeof item === "number" && Number.isSafeInteger(item)) ||
            (typeof item === "string" && /^[a-zA-Z0-9._\/:\-]{1,64}$/.test(item));
          if (value.event === "stream_warning" && typeof value.stream === "string" &&
              /^(rtsp-[0-9a-f]{16}|nvhttp-[0-9a-f]{32}-[0-9a-f]{16}-rtsp|stream-session-[0-9a-f]{16})$/.test(value.stream) &&
              warning && typeof warning.code === "string" && /^[a-zA-Z0-9._\/:\-]{1,64}$/.test(warning.code) &&
              typeof warning.source === "string" && /^[a-zA-Z0-9._\/:\-]{1,128}$/.test(warning.source) &&
              typeof warning.reason === "string" && /^[a-zA-Z0-9._\/:\-]{1,64}$/.test(warning.reason) &&
              (warning.os_error === undefined || (Number.isInteger(warning.os_error) && warning.os_error >= -2147483648 && warning.os_error <= 2147483647)) &&
              scalar(warning.requested) && scalar(warning.effective)) {
            const key = `warning:${value.stream}:${warning.code}:${warning.reason}`;
            const existing = alerts.get(key);
            if (existing) { existing.occurrences++; existing.lastSeen = warningLine[1]!; }
            else {
              if (alerts.size >= 32) alerts.delete(alerts.keys().next().value!);
              alerts.set(key, {key, featureFlags:"", occurrences:1, lastSeen:warningLine[1]!, warning:{stream:value.stream, ...warning}});
            }
          }
        } catch { /* Ignore malformed log records. */ }
        continue;
      }
      const fallback = /^\[([^\]\r\n]{1,64})\]: Warning: (?:amaterasu\.rtsp::rtsp|amaterasu\.stream-session::stream)\.compatibility: (\{.{1,2048}\})$/.exec(line.replace(/\r$/, ""));
      if (fallback) {
        try {
          const value = JSON.parse(fallback[2]!);
          if (value.event === "stream_adjustment" && typeof value.stream === "string" && /^(rtsp-[0-9a-f]{16}|nvhttp-[0-9a-f]{32}-[0-9a-f]{16}-rtsp|stream-session-[0-9a-f]{16})$/.test(value.stream) &&
              ["refresh_hint_ignored", "intra_refresh_unavailable", "audio_quality_default", "hevc_capability_hint_ignored", "invalid_optional_hints", "optional_client_features", "bandwidth_adjusted", "compatibility_defaults", "capture_fallback", "encoder_fallback"].includes(value.code) &&
              (value.reason === undefined || (typeof value.reason === "string" && /^[a-zA-Z0-9_-]{1,128}$/.test(value.reason))) &&
              Number.isSafeInteger(value.requested) && value.requested >= 0 && value.requested <= 4294967295 &&
              Number.isSafeInteger(value.effective) && value.effective >= 0 && value.effective <= 4294967295) {
            const key = `adjustment:${value.stream}:${value.code}:${value.requested}:${value.effective}`;
            const existing = alerts.get(key);
            if (existing) { existing.occurrences++; existing.lastSeen = fallback[1]!; }
            else {
              if (alerts.size >= 32) alerts.delete(alerts.keys().next().value!);
              alerts.set(key, {key, featureFlags: "", occurrences: 1, lastSeen: fallback[1]!, adjustment: value});
            }
          }
        } catch { /* Ignore malformed log records. */ }
        continue;
      }
      const match = pattern.exec(line.replace(/\r$/, ""));
      if (!match) continue;
      const flags = match[2]!;
      const numeric = Number.parseInt(flags.slice(2), 16);
      if (numeric === 0 || (numeric & 15) !== 0) continue;
      const key = `optional-client-features:${flags}`;
      const existing = alerts.get(key);
      if (existing) { existing.occurrences++; existing.lastSeen = match[1]!; }
      else {
        if (alerts.size >= 32) alerts.delete(alerts.keys().next().value!);
        alerts.set(key, { key, featureFlags: flags, occurrences: 1, lastSeen: match[1]! });
      }
    }
    return [...alerts.values()];
  }

  export interface DiagnosticAlertObserver {
    snapshot(contents: string): void;
    unavailable(): void;
  }

  export function renderDiagnosticAlerts(context: PageContext, target: HTMLElement): DiagnosticAlertObserver {
    const storageKey = `amaterasu.diagnostic-alert-dismissals.v1.${encodeURIComponent(context.api.hostOrigin)}`;
    const dismissed = new Set<string>();
    let persistent = true;
    let alerts: StreamCompatibilityAlert[] = [];
    let state: "loading" | "ready" | "unavailable" = "loading";
    try {
      const stored: unknown = JSON.parse(window.localStorage.getItem(storageKey) ?? "[]");
      if (Array.isArray(stored)) for (const key of stored.slice(-64)) {
        if (typeof key === "string" && /^(optional-client-features:0x[0-9a-f]{8}|adjustment:(?:rtsp-[0-9a-f]{16}|nvhttp-[0-9a-f]{32}-[0-9a-f]{16}-rtsp|stream-session-[0-9a-f]{16}):[a-z_]+:[0-9]+:[0-9]+|warning:(?:rtsp-[0-9a-f]{16}|nvhttp-[0-9a-f]{32}-[0-9a-f]{16}-rtsp|stream-session-[0-9a-f]{16}):[a-zA-Z0-9._\/\-]+:[a-zA-Z0-9._\/\-]+)$/.test(key)) dismissed.add(key);
      }
    } catch { persistent = false; }

    function save(): void {
      while (dismissed.size > 64) dismissed.delete(dismissed.values().next().value!);
      try { window.localStorage.setItem(storageKey, JSON.stringify([...dismissed])); }
      catch { persistent = false; }
    }

    function render(): void {
      const list = el("div");
      list.setAttribute("aria-live", "polite");
      const visible = alerts.filter((alert) => !dismissed.has(alert.key));
      for (const alert of visible) {
        if (alert.warning) {
          const warning = alert.warning;
          const message = warning.code === "proton_policy_unavailable"
            ? "The global controller compatibility preference was unavailable. The recorded fallback applies where no app or client override is set."
            : warning.code === "proton_hook_unavailable"
              ? "The optional Proton controller compatibility helper could not be confirmed. The application continued launching."
              : warning.code === "application_output_unavailable"
                ? "The configured application output could not be used. The application continued without that output."
              : warning.code === "detached_command_unavailable"
                ? "An optional detached command could not start. The application continued launching."
              : warning.code === "invalid_client_hint"
                ? "An invalid optional client hint was ignored. The stream settings were preserved."
              : "An optional stream feature was unavailable. Streaming continued.";
          list.append(el("article", "notice", el("h3", "", "Stream continued with a warning"),
            el("p", "", message),
            el("p", "muted", `Stream: ${warning.stream}. Source: ${warning.source}. Reason: ${warning.reason}.${warning.os_error === undefined ? "" : ` OS error: ${warning.os_error}.`}${warning.requested === undefined ? "" : ` Requested: ${warning.requested}.`}${warning.effective === undefined ? "" : ` Used: ${warning.effective}.`} Last seen: ${alert.lastSeen}.`),
            button("Dismiss", () => { dismissed.add(alert.key); save(); render(); })));
          continue;
        }
        if (alert.adjustment) {
          const adjustment = alert.adjustment;
          const messages: Record<string, string> = {
            refresh_hint_ignored: "An inconsistent display refresh hint was ignored. The requested stream frame rate is used.",
            intra_refresh_unavailable: "Intra refresh is unavailable. Streaming continues with the default refresh behavior.",
            audio_quality_default: "The requested audio quality is unavailable. Streaming continues with the configured audio quality.",
            hevc_capability_hint_ignored: "An unrecognized HEVC capability hint was ignored. The negotiated video format is unchanged.",
            invalid_optional_hints: "Invalid optional client hints were ignored. Required media and security settings are unchanged.",
            optional_client_features: "Unsupported optional client features were ignored.",
            bandwidth_adjusted: "The video bitrate was reduced to allow room for audio and network overhead.",
            capture_fallback: "The capture provider could not initialize. Streaming continues with a compatible capture path.",
            encoder_fallback: "The encoder could not initialize. Streaming continues with a compatible encoder.",
            compatibility_defaults: "The client omitted optional settings. Compatible defaults were used.",
          };
          list.append(el("article", "notice", el("h3", "", "Stream continued with adjusted settings"),
            el("p", "", messages[adjustment.code]!),
            el("p", "muted", adjustment.code === "invalid_optional_hints"
              ? `Stream: ${adjustment.stream}. Ignored hint mask: 0x${adjustment.requested.toString(16)}. Last seen: ${alert.lastSeen}.`
              : `Stream: ${adjustment.stream}. Requested: ${adjustment.requested}; used: ${adjustment.effective}.${adjustment.reason ? ` Reason: ${adjustment.reason}.` : ""} Last seen: ${alert.lastSeen}.`),
            button("Dismiss", () => { dismissed.add(alert.key); save(); render(); })));
          continue;
        }
        const dismiss = button("Dismiss", () => { dismissed.add(alert.key); save(); render(); });
        dismiss.setAttribute("aria-label", `Dismiss optional client feature alert ${alert.featureFlags}`);
        list.append(el("article", "notice",
          el("h3", "", "Some optional client features are unavailable"),
          el("p", "", "Streaming can continue. Amaterasu ignored optional features advertised by your client that this host does not recognize."),
          el("p", "", "If you need these features, check for host and client updates. Otherwise, no action is needed."),
          el("p", "muted", `Feature flags: ${alert.featureFlags}. Seen ${alert.occurrences} time(s) in the retained host log. Last seen: ${alert.lastSeen}.`),
          dismiss));
      }
      if (!visible.length && state === "ready") list.append(el("p", "muted", "No undismissed stream compatibility alerts in the retained host log."));
      if (state === "loading") list.append(el("p", "muted", "Checking the retained host log for stream compatibility alerts…"));
      if (state === "unavailable") list.append(el("p", "notice", context.api.allows("host-logs.read")
        ? "Alerts could not be refreshed. Previously loaded alerts remain visible. Use Refresh to try again."
        : "Host-log read access is required to view stream compatibility alerts."));
      const restore = button("Restore dismissed alerts", () => { dismissed.clear(); save(); render(); });
      restore.disabled = dismissed.size === 0;
      target.replaceChildren(panel("Alerts", list, restore,
        el("p", "muted", persistent
          ? "Dismissals are saved for this host in this browser. Dismissing an alert does not change streaming or delete logs. Different feature flags can raise a new alert."
          : "Browser storage is unavailable. Dismissals last only while this page is open; streaming and logs are unchanged."),
        el("p", "muted", "Alerts cover reported stream adjustments and optional-feature warnings in the retained host log. They refresh with the host log; paused log refresh also pauses new alerts.")));
    }
    if (!context.api.allows("host-logs.read")) state = "unavailable";
    render();
    return {
      snapshot(contents) { alerts = streamCompatibilityAlerts(contents); state = "ready"; render(); },
      unavailable() { state = "unavailable"; render(); },
    };
  }
}
