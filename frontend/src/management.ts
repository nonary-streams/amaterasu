namespace Amaterasu {
  interface PendingPairing {
    pairing_id: string;
    client_id: string;
    name: string;
    expires_at: number;
    awaiting_pin: boolean;
  }

  interface PendingPairingsReply {
    ok: true;
    pending: PendingPairing[];
  }

  interface HostStatusReply {
    ok: true;
    hostname: string;
    revision: number;
    active_http_port: number | null;
    active_https_port: number | null;
    active_rtsp_port: number | null;
    listener_error: string | null;
    rtsp_listener_error: string | null;
  }

  interface SessionStatusReply {
    ok: true;
    sessions: Session[];
    next_offset: number | null;
  }

  interface RuntimeStatusReply {
    ok: true;
    status: {
      available: boolean;
      configuration_ready: boolean;
      active_streams: number;
      failed_operations: number;
    };
  }

  export function parsePendingPairings(value: unknown): PendingPairingsReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Pending pairing response is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Array.isArray(reply.pending))
      throw new Error("Pending pairing response is invalid.");
    const pending = reply.pending.map((item): PendingPairing => {
      if (!item || typeof item !== "object" || Array.isArray(item))
        throw new Error("Pending pairing entry is invalid.");
      const entry = item as Record<string, unknown>;
      if (
        typeof entry.pairing_id !== "string" ||
        !/^[a-f0-9]{32}$/.test(entry.pairing_id) ||
        typeof entry.client_id !== "string" ||
        entry.client_id.length < 1 ||
        entry.client_id.length > 64 ||
        /[\u0000-\u001f\u007f-\u009f]/.test(entry.client_id) ||
        typeof entry.name !== "string" ||
        entry.name.length < 1 ||
        entry.name.length > 128 ||
        /[\u0000-\u001f\u007f-\u009f]/.test(entry.name) ||
        !Number.isSafeInteger(entry.expires_at) ||
        (entry.expires_at as number) < 0 ||
        typeof entry.awaiting_pin !== "boolean"
      )
        throw new Error("Pending pairing entry is invalid.");
      return {
        pairing_id: entry.pairing_id,
        client_id: entry.client_id,
        name: entry.name,
        expires_at: entry.expires_at as number,
        awaiting_pin: entry.awaiting_pin,
      };
    });
    return { ok: true, pending };
  }

  function parseHostStatus(value: unknown): HostStatusReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Host status response is invalid.");
    const reply = value as Record<string, unknown>;
    if (
      reply.ok !== true ||
      typeof reply.hostname !== "string" ||
      !Number.isSafeInteger(reply.revision) ||
      !nullablePort(reply.active_http_port) ||
      !nullablePort(reply.active_https_port) ||
      !nullablePort(reply.active_rtsp_port) ||
      !nullableText(reply.listener_error) ||
      !nullableText(reply.rtsp_listener_error)
    )
      throw new Error("Host status response is invalid.");
    return reply as unknown as HostStatusReply;
  }

  function nullablePort(value: unknown): value is number | null {
    return value === null ||
      (Number.isSafeInteger(value) && (value as number) >= 1 && (value as number) <= 65535);
  }

  function nullableText(value: unknown): value is string | null {
    return value === null || typeof value === "string";
  }

  function parseSessionStatus(value: unknown): SessionStatusReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Session status response is invalid.");
    const reply = value as Record<string, unknown>;
    if (
      reply.ok !== true ||
      !Array.isArray(reply.sessions) ||
      !(reply.next_offset === null ||
        (Number.isSafeInteger(reply.next_offset) && (reply.next_offset as number) >= 0))
    )
      throw new Error("Session status response is invalid.");
    for (const item of reply.sessions) {
      if (!item || typeof item !== "object" || Array.isArray(item))
        throw new Error("Session status entry is invalid.");
      const session = item as Record<string, unknown>;
      if (
        !Number.isSafeInteger(session.id) ||
        (session.id as number) < 0 ||
        typeof session.key !== "string" ||
        typeof session.state !== "string" ||
        !["preparing", "active", "stopping", "stopped"].includes(session.state) ||
        !(session.outcome === null || session.outcome === undefined ||
          ["completed", "cancelled", "failed"].includes(String(session.outcome))) ||
        !(session.lease_count === undefined ||
          (Number.isSafeInteger(session.lease_count) && (session.lease_count as number) >= 0))
      )
        throw new Error("Session status entry is invalid.");
    }
    return reply as unknown as SessionStatusReply;
  }

  function parseRuntimeStatus(value: unknown): RuntimeStatusReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Streaming status response is invalid.");
    const reply = value as Record<string, unknown>;
    if (!reply.status || typeof reply.status !== "object" || Array.isArray(reply.status))
      throw new Error("Streaming status response is invalid.");
    const status = reply.status as Record<string, unknown>;
    if (
      reply.ok !== true ||
      typeof status.available !== "boolean" ||
      typeof status.configuration_ready !== "boolean" ||
      !Number.isSafeInteger(status.active_streams) ||
      (status.active_streams as number) < 0 ||
      !Number.isSafeInteger(status.failed_operations) ||
      (status.failed_operations as number) < 0
    )
      throw new Error("Streaming status response is invalid.");
    return reply as unknown as RuntimeStatusReply;
  }

  export function renderHostManagement(context: PageContext): void {
    const pairingArea = el("div");
    const hostArea = el("div");
    const sessionArea = el("div");
    const runtimeArea = el("div");
    let pairingRequest = 0;
    let hostRequest = 0;
    let sessionRequest = 0;
    let runtimeRequest = 0;

    context.content.append(
      sectionHeading(
        "Host & devices",
        "Review pending device access and the status reported by this host.",
        button("Refresh all", () => {
          void loadPairings();
          void loadHost();
          void loadSessions();
          void loadRuntime();
        }),
      ),
      panel("Pending device pairing", pairingArea),
      panel("Host listeners", hostArea),
      panel("Streaming status", runtimeArea),
      panel("Management sessions", sessionArea),
    );

    async function loadPairings(): Promise<void> {
      const request = ++pairingRequest;
      if (!context.api.allows("pairing.admin")) {
        pairingArea.replaceChildren(
          el("p", "muted", "This account does not have permission to review or approve device pairing requests."),
        );
        return;
      }
      pairingArea.replaceChildren(el("p", "muted", "Loading pending pairing requests…"));
      try {
        const reply = parsePendingPairings(
          await context.api.post<unknown>("pairing/pending"),
        );
        if (!context.content.isConnected || request !== pairingRequest) return;
        if (reply.pending.length === 0) {
          pairingArea.replaceChildren(
            empty("No pending requests", "New device requests will appear here when this host receives them."),
            button("Refresh requests", () => { void loadPairings(); }),
          );
          return;
        }
        const cards = reply.pending.map((pending) => renderPendingPairing(pending));
        pairingArea.replaceChildren(...cards, button("Refresh requests", () => { void loadPairings(); }));
      } catch (error) {
        if (context.content.isConnected && request === pairingRequest)
          pairingArea.replaceChildren(errorPanel(error, loadPairings));
      }
    }

    function renderPendingPairing(pending: PendingPairing): HTMLElement {
      const card = el("article", "panel pairing-request");
      const description = el(
        "p",
        "muted",
        pending.awaiting_pin
          ? "Enter the four-digit PIN shown on the requesting device to approve this pairing step."
          : "PIN approval was accepted. The device must complete its pairing exchange.",
      );
      const status = el("div");
      status.setAttribute("aria-live", "polite");
      const details = el(
        "div",
        "pairing-details",
        el("strong", "", pending.name),
        el("small", "muted block", `Client ${pending.client_id}`),
        el("small", "muted block", `Request expires ${new Date(pending.expires_at * 1000).toLocaleString()}`),
        badge(pending.awaiting_pin ? "PIN required" : "Pairing in progress", pending.awaiting_pin ? "accent" : "good"),
      );
      card.append(details, description);
      if (pending.awaiting_pin) {
        const pin = input("pairing_pin", "", "password");
        pin.autocomplete = "off";
        pin.spellcheck = false;
        pin.inputMode = "numeric";
        pin.maxLength = 4;
        pin.pattern = "[0-9]{4}";
        pin.required = true;
        const approve = el("button", "button primary", "Approve PIN");
        approve.type = "submit";
        const form = el("form", "pairing-approval", field("Four-digit PIN", pin), status, approve);
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          const submittedPin = pin.value;
          pin.value = "";
          if (!/^\d{4}$/.test(submittedPin)) {
            status.replaceChildren(errorPanel(new Error("Enter the four-digit PIN shown on the device.")));
            return;
          }
          void runAction(approve, status, async () => {
            await context.api.post("pairing/approve_pin", {
              pairing_id: pending.pairing_id,
              pin: submittedPin,
            });
            context.notify("PIN approval accepted. The device must complete its pairing exchange.");
            await loadPairings();
          });
        });
        card.append(form);
      } else {
        card.append(status);
      }
      return card;
    }

    async function loadHost(): Promise<void> {
      const request = ++hostRequest;
      if (!context.api.allows("configuration.read")) {
        hostArea.replaceChildren(el("p", "muted", "Host settings status requires configuration.read permission."));
        return;
      }
      hostArea.replaceChildren(el("p", "muted", "Loading host listener status…"));
      try {
        const host = parseHostStatus(await context.api.post<unknown>("nvhttp/settings"));
        if (!context.content.isConnected || request !== hostRequest) return;
        const listener = (label: string, port: number | null, error: string | null) => [
          el("strong", "", label),
          port === null
            ? badge(error ? "Error reported" : "Not confirmed")
            : badge(`Active · port ${port}`, "good"),
        ];
        const rows = [
          listener("HTTP", host.active_http_port, host.listener_error),
          listener("HTTPS", host.active_https_port, host.listener_error),
          listener("RTSP", host.active_rtsp_port, host.rtsp_listener_error),
        ];
        hostArea.replaceChildren(
          el("p", "muted", `Host ${host.hostname} · settings revision ${host.revision}`),
          table(["Listener", "Reported status"], rows),
          button("Refresh host status", () => { void loadHost(); }),
        );
      } catch (error) {
        if (context.content.isConnected && request === hostRequest)
          hostArea.replaceChildren(errorPanel(error, loadHost));
      }
    }

    async function loadSessions(): Promise<void> {
      const request = ++sessionRequest;
      if (!context.api.allows("session.read")) {
        sessionArea.replaceChildren(el("p", "muted", "Management session status requires session.read permission."));
        return;
      }
      sessionArea.replaceChildren(el("p", "muted", "Loading management sessions…"));
      try {
        const reply = parseSessionStatus(await context.api.post<unknown>("session/list", { limit: 16 }));
        if (!context.content.isConnected || request !== sessionRequest) return;
        const rows = reply.sessions.map((session) => [
          el("strong", "", session.key),
          badge(session.state, session.state === "active" ? "good" : ""),
          String(session.lease_count ?? Object.keys(session.leases ?? {}).length),
          session.outcome ?? "—",
        ]);
        sessionArea.replaceChildren(
          rows.length
            ? table(["Session", "State", "Leases", "Outcome"], rows)
            : empty("No management sessions", "Sessions created through the management API will appear here."),
          reply.next_offset === null
            ? el("p", "muted", `Showing ${reply.sessions.length} session${reply.sessions.length === 1 ? "" : "s"}.`)
            : el("p", "muted", `Showing the first ${reply.sessions.length} sessions; more are available.`),
          button("Refresh sessions", () => { void loadSessions(); }),
        );
      } catch (error) {
        if (context.content.isConnected && request === sessionRequest)
          sessionArea.replaceChildren(errorPanel(error, loadSessions));
      }
    }

    async function loadRuntime(): Promise<void> {
      const request = ++runtimeRequest;
      if (!context.api.allows("session.read")) {
        runtimeArea.replaceChildren(el("p", "muted", "Streaming status requires session.read permission."));
        return;
      }
      runtimeArea.replaceChildren(el("p", "muted", "Loading streaming status…"));
      try {
        const reply = parseRuntimeStatus(await context.api.post<unknown>("history/history_status"));
        if (!context.content.isConnected || request !== runtimeRequest) return;
        const state = reply.status;
        runtimeArea.replaceChildren(
          badge(state.available && state.configuration_ready ? "History service available" : "History service unavailable", state.available && state.configuration_ready ? "good" : ""),
          el("p", "muted", `${state.active_streams} active stream${state.active_streams === 1 ? "" : "s"} reported · ${state.failed_operations} failed storage operation${state.failed_operations === 1 ? "" : "s"} since service start.`),
          button("Refresh streaming status", () => { void loadRuntime(); }),
        );
      } catch (error) {
        if (context.content.isConnected && request === runtimeRequest)
          runtimeArea.replaceChildren(errorPanel(error, loadRuntime));
      }
    }

    void loadPairings();
    void loadHost();
    void loadSessions();
    void loadRuntime();
  }
}
