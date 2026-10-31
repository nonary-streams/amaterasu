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
    const pairedClientsArea = el("div");
    const drafts = context.registerDraftGuard ? trackDrafts(pairedClientsArea) : null;
    let clientSaveBusy = false;
    if (drafts) context.registerDraftGuard?.(() => clientSaveBusy || drafts.isDirty(), drafts.dispose);
    const loadPairedClients = createPairedClientEditor(context, pairedClientsArea, () => drafts, (busy) => {
      clientSaveBusy = busy;
    });
    const captureArea = el("div");
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
          if (context.api.allows("pairing.admin")) {
            if (clientSaveBusy) context.notify("Wait for the client save to finish before refreshing.");
            else if (!drafts?.isDirty() || window.confirm("Reload paired clients and discard unsaved client changes?"))
              void loadPairedClients();
          }
          void loadHost();
          void loadSessions();
          void loadRuntime();
        }),
      ),
      panel("Pending device pairing", pairingArea),
      pairedClientsArea,
      panel("KDE desktop capture consent", captureArea),
      panel("Host listeners", hostArea),
      panel("Streaming status", runtimeArea),
      panel("Session coordination", sessionArea),
    );

    async function loadPairings(notice?: string): Promise<void> {
      if (!context.content.isConnected) return;
      const request = ++pairingRequest;
      if (!context.api.allows("pairing.admin")) {
        pairingArea.replaceChildren(
          el("p", "muted", "This account does not have permission to review or approve device pairing requests."),
        );
        return;
      }
      pairingArea.replaceChildren(
        ...(notice ? [pairingApprovalNotice(notice)] : []),
        el("p", "muted", "Loading pending pairing requests…"),
      );
      try {
        const reply = parsePendingPairings(
          await context.api.post<unknown>("pairing/pending"),
        );
        if (!context.content.isConnected || request !== pairingRequest) return;
        if (reply.pending.length === 0) {
          pairingArea.replaceChildren(
            ...(notice ? [pairingApprovalNotice(notice)] : []),
            empty("No pending requests", "New device requests will appear here when this host receives them."),
            button("Refresh requests", () => { void loadPairings(); }),
          );
          return;
        }
        const cards = reply.pending.map((pending) => renderPendingPairing(pending));
        pairingArea.replaceChildren(
          ...(notice ? [pairingApprovalNotice(notice)] : []),
          ...cards,
          button("Refresh requests", () => { void loadPairings(); }),
        );
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
          if (approve.disabled) return;
          const submittedPin = pin.value;
          pin.value = "";
          if (!/^\d{4}$/.test(submittedPin)) {
            status.replaceChildren(errorPanel(new Error("Enter the four-digit PIN shown on the device.")));
            return;
          }
          void runAction(approve, status, async () => {
            try {
              await context.api.post("pairing/approve_pin", {
                pairing_id: pending.pairing_id,
                pin: submittedPin,
              });
            } catch {
              if (!context.content.isConnected) return;
              const notice = "The approval response was not confirmed. The PIN was cleared and current pairing status was refreshed; review it before trying again.";
              context.notify("Approval was not confirmed. Current pairing status is being refreshed; the PIN was cleared.");
              await loadPairings(notice);
              return;
            }
            if (!context.content.isConnected) return;
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
        sessionArea.replaceChildren(el("p", "muted", "Session coordination requires session.read permission."));
        return;
      }
      sessionArea.replaceChildren(el("p", "muted", "Loading session coordination…"));
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
            : empty("No coordination sessions", "Sessions created through the session API will appear here."),
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
    if (context.api.allows("pairing.admin")) void loadPairedClients();
    else pairedClientsArea.append(panel("Paired clients", el("p", "muted", "This account does not have permission to manage paired clients.")));
    renderCaptureConsent(context, captureArea);
    void loadHost();
    void loadSessions();
    void loadRuntime();
  }

  function pairingApprovalNotice(message: string): HTMLElement {
    const notice = el("div", "notice", el("strong", "", "Approval not confirmed"), el("p", "", message));
    notice.setAttribute("role", "status");
    notice.setAttribute("aria-live", "polite");
    return notice;
  }

  const PORTAL_POLL_INTERVAL_MS = 1000;
  const PORTAL_POLL_LIMIT = 120;

  function renderCaptureConsent(context: PageContext, area: HTMLElement): void {
    let operationId: string | null = null;
    let pollTimer: number | null = null;
    let pollCount = 0;
    let requestInFlight = false;
    let finished = false;
    let observer: MutationObserver | null = null;
    const cleanedOperations = new Set<string>();

    const status = el("div");
    status.setAttribute("role", "status");
    status.setAttribute("aria-live", "polite");
    const choose = button("Choose monitor in KDE…", () => { void beginSelection(); }, "primary");
    const cancel = button("Cancel selection", () => { void cancelSelection(); });
    const permission = el("p", "muted");
    const help = el(
      "p",
      "muted",
      "This opens the KDE desktop portal only after you choose a monitor. After it reports ready, launch Moonlight Desktop promptly: the selected source is retained for up to 60 seconds. Selection does not confirm that a stream started.",
    );
    area.append(permission, help, choose, cancel, status);
    cancel.hidden = true;
    updatePermission();

    function updatePermission(): void {
      const canSelect = context.api.allows("capture.write");
      const canPoll = context.api.allows("capture.read");
      choose.disabled = !canSelect || !canPoll || Boolean(operationId) || !context.content.isConnected;
      cancel.disabled = !canSelect || !operationId || requestInFlight;
      permission.textContent = !canSelect
        ? "Monitor selection and cancellation require capture.write permission."
        : !canPoll
          ? "Selection status requires capture.read permission; no chooser will be opened."
          : "Monitor selection requires capture.write and capture.read permissions.";
      cancel.hidden = !operationId;
    }

    function show(message: string, tone = "muted"): void {
      status.replaceChildren(el("p", tone, message));
    }

    function newOperationId(): string {
      const bytes = new Uint8Array(16);
      window.crypto.getRandomValues(bytes);
      let suffix = "";
      for (const byte of bytes) suffix += byte.toString(16).padStart(2, "0");
      return `desktop-${suffix}`;
    }

    function armUnmountCleanup(): void {
      observer?.disconnect();
      observer = new MutationObserver(() => {
        if (!context.content.isConnected) {
          observer?.disconnect();
          observer = null;
          stopPolling();
          void cancelForCleanup();
        }
      });
      observer.observe(document, { childList: true, subtree: true });
    }

    function stopPolling(): void {
      if (pollTimer !== null) window.clearTimeout(pollTimer);
      pollTimer = null;
      observer?.disconnect();
      observer = null;
    }

    function finish(message: string, tone = "muted"): void {
      finished = true;
      stopPolling();
      show(message, tone);
      operationId = null;
      requestInFlight = false;
      updatePermission();
    }

    function schedulePoll(): void {
      if (finished || !operationId || !context.content.isConnected) return;
      if (pollCount >= PORTAL_POLL_LIMIT) {
        const expiredOperation = operationId;
        finish("Monitor selection timed out. Start a new selection if you still want to choose a monitor.", "error");
        void cancelForCleanup(expiredOperation);
        return;
      }
      pollTimer = window.setTimeout(() => { void pollSelection(); }, PORTAL_POLL_INTERVAL_MS);
    }

    function selectionErrorMessage(error: unknown): { message: string; tone: string } {
      const code = error && typeof error === "object" && "code" in error
        ? String((error as { code: unknown }).code)
        : "";
      if (["source_selection_expired", "source_selection_timeout", "portal_selection_expired"].includes(code))
        return { message: "The KDE consent request expired. Start a new selection to try again.", tone: "error" };
      if (["source_selection_denied", "selection_permission_denied", "portal_selection_denied"].includes(code))
        return { message: "KDE denied desktop capture consent. No source was selected.", tone: "error" };
      if (["source_selection_cancelled", "portal_selection_cancelled"].includes(code))
        return { message: "Monitor selection was cancelled.", tone: "muted" };
      if (["portal_unavailable", "pipewire_unavailable"].includes(code))
        return { message: "The KDE desktop portal is unavailable. Check that a desktop session is active, then try again.", tone: "error" };
      return { message: "Unable to read monitor selection status. Check the connection and refresh before starting another selection.", tone: "error" };
    }

    function validateReply(value: unknown): Record<string, unknown> {
      if (!value || typeof value !== "object" || Array.isArray(value))
        throw new Error("invalid selection response");
      const reply = value as Record<string, unknown>;
      if (reply.ok !== true || reply.operation_id !== operationId || reply.source_type !== "monitor" ||
          !["selecting", "ready", "denied", "failed"].includes(String(reply.selection_state)))
        throw new Error("invalid selection response");
      return reply;
    }

    async function beginSelection(): Promise<void> {
      if (operationId || !context.content.isConnected) return;
      updatePermission();
      if (choose.disabled || !context.api.allows("capture.write") || !context.api.allows("capture.read")) return;
      let createdId: string;
      try {
        createdId = newOperationId();
      } catch {
        show("Secure operation ID generation is unavailable; no chooser was opened.", "error");
        return;
      }
      operationId = createdId;
      pollCount = 0;
      finished = false;
      requestInFlight = true;
      show("Opening the KDE monitor consent prompt…");
      updatePermission();
      armUnmountCleanup();
      try {
        const reply = validateReply(await context.api.post<unknown>("capture/select_portal_source", {
          source_type: "monitor",
          operation_id: createdId,
        }));
        if (!context.content.isConnected || operationId !== createdId) {
          void cancelForCleanup(createdId);
          return;
        }
        requestInFlight = false;
        updatePermission();
        acceptReply(reply);
      } catch (error) {
        if (operationId !== createdId || !context.content.isConnected) {
          void cancelForCleanup(createdId);
          return;
        }
        const outcome = selectionErrorMessage(error);
        finish(outcome.message, outcome.tone);
        void cancelForCleanup(createdId);
      }
    }

    function acceptReply(reply: Record<string, unknown>): void {
      if (reply.selection_state === "ready") {
        finish("KDE selected a monitor. Launch Moonlight Desktop promptly within the 60-second selection lifetime. This confirms selection only, not a running stream.", "good");
      } else if (reply.selection_state === "denied") {
        finish("KDE denied desktop capture consent. No source was selected.", "error");
      } else if (reply.selection_state === "failed") {
        const error = { code: reply.error };
        const outcome = selectionErrorMessage(error);
        finish(outcome.message, outcome.tone);
      } else {
        show("Selecting monitor… Approve the request in the KDE desktop portal. When it reports ready, launch Moonlight Desktop promptly within 60 seconds.");
        schedulePoll();
      }
    }

    async function pollSelection(): Promise<void> {
      const current = operationId;
      if (!current || finished || requestInFlight || !context.content.isConnected) return;
      if (!context.api.allows("capture.read")) {
        finish("Monitor selection status is unavailable because capture.read permission is missing.", "error");
        void cancelForCleanup(current);
        return;
      }
      requestInFlight = true;
      pollCount += 1;
      updatePermission();
      try {
        const reply = validateReply(await context.api.post<unknown>("capture/poll_portal_selection", {
          operation_id: current,
        }));
        if (!context.content.isConnected || operationId !== current) {
          void cancelForCleanup(current);
          return;
        }
        requestInFlight = false;
        updatePermission();
        acceptReply(reply);
      } catch (error) {
        if (!context.content.isConnected || operationId !== current) {
          void cancelForCleanup(current);
          return;
        }
        const outcome = selectionErrorMessage(error);
        if (outcome.message.startsWith("Unable to read")) {
          requestInFlight = false;
          updatePermission();
          show(`${outcome.message} Retrying while this request remains active.`, "error");
          schedulePoll();
        } else {
          finish(outcome.message, outcome.tone);
        }
      }
    }

    async function cancelSelection(): Promise<void> {
      const current = operationId;
      if (!current || requestInFlight || !context.api.allows("capture.write")) return;
      stopPolling();
      requestInFlight = true;
      updatePermission();
      show("Cancelling monitor selection…");
      try {
        await context.api.post<unknown>("capture/cancel_portal_selection", { operation_id: current });
        if (!context.content.isConnected || operationId !== current) return;
        finish("Monitor selection was cancelled.");
      } catch {
        if (!context.content.isConnected || operationId !== current) return;
        requestInFlight = false;
        updatePermission();
        show("Cancellation was not confirmed. Retry cancellation before leaving this page.", "error");
        armUnmountCleanup();
      }
    }

    async function cancelForCleanup(id = operationId): Promise<void> {
      if (!id || cleanedOperations.has(id) || !context.api.allows("capture.write")) return;
      cleanedOperations.add(id);
      try {
        await context.api.post<unknown>("capture/cancel_portal_selection", { operation_id: id });
      } catch {
        // The view is gone or the request already ended; cleanup must not leak provider details.
      }
      if (operationId === id) {
        operationId = null;
        finished = true;
        requestInFlight = false;
      }
    }
  }
}
