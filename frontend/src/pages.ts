namespace Amaterasu {
  function pagesTextLiteral(value: string): TextValue {
    return typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(value) : value;
  }

  function pagesTextMessage(value: string): TextValue {
    return typeof Amaterasu.localizedMessage === "function" ? Amaterasu.localizedMessage(value) : value;
  }

  export function renderOverview(context: PageContext): void {
    const area = el("div");
    const activityArea = el("div", "overview-activity-area");
    const hostMetricsArea = el("div", "overview-plugin-sections");
    area.append(el("p", "muted overview-loading", "Loading host information…"));
    let refreshGeneration = 0;
    let refreshActivity = (): void => {};
    context.content.append(
      sectionHeading(
        "Host overview",
        "Your applications, sessions, and local services in one place.",
        button("Refresh", () => { void load(); refreshActivity(); }),
      ),
      activityArea,
      hostMetricsArea,
      area,
    );
    refreshActivity = renderOverviewActivity(context, activityArea);
    renderExtensionSlot(context, hostMetricsArea, "overview");
    async function load(): Promise<void> {
      const generation = ++refreshGeneration;
      const snapshots = await Promise.allSettled([
        context.api.post<CatalogReply>("catalog/list", { limit: 32 }),
        context.api.post<SessionReply>("session/list", { limit: 16 }),
        context.api.post<ProcessReply>("application/list", { limit: 16 }),
        context.api.post<SteamStatus>("steam/status"),
      ]);
      if (!context.content.isConnected || generation !== refreshGeneration) return;
      const [catalog, sessions, applications, steam] = snapshots;
      const metrics = el("div", "metrics");
      const addMetric = (label: string, value: string, detail: string) =>
        metrics.append(
          el(
            "div",
            "metric",
            el("span", "muted", label),
            el("strong", "", pagesTextLiteral(value)),
            el("small", "muted", detail),
          ),
        );
      addMetric(
        "Catalog applications",
        catalog?.status === "fulfilled"
          ? countLabel(catalog.value.entries.length, catalog.value.next_cursor)
          : "—",
        "Catalog entries returned",
      );
      addMetric(
        "Session records",
        sessions?.status === "fulfilled"
          ? countLabel(
              sessions.value.sessions.length,
              sessions.value.next_offset,
            )
          : "—",
        "Caller-scoped coordination records",
      );
      addMetric(
        "Tracked processes",
        applications?.status === "fulfilled"
          ? countLabel(
              applications.value.processes.length,
              applications.value.next_offset,
            )
          : "—",
        "Running and recently exited",
      );
      addMetric(
        "Steam discoveries",
        steam?.status === "fulfilled" ? String(steam.value.game_count) : "—",
        steam?.status === "fulfilled" && steam.value.scanning
          ? "Scanning local libraries"
          : "Latest provider scan",
      );
      const available = snapshots.filter((snapshot) => snapshot.status === "fulfilled").length;
      const status = el(
        "section", "overview-status",
        el("div", "overview-status-icon", available === snapshots.length ? "✓" : "!"),
        el("div", "overview-status-copy",
          el("p", "overview-status-label", "MANAGEMENT SERVICE ACCESS"),
          el("h2", "", available === snapshots.length ? "Management services responding" : "Some information is unavailable"),
          el("p", "muted", available === snapshots.length
            ? "Catalog, session coordination, applications, and Steam responded to the latest refresh. This does not indicate stream readiness."
            : "Some management requests could not be completed. Check service access below for details.")),
      );
      const quickActions = el("div", "quick-actions");
      for (const [page, label, detail, scope] of [
        ["host", "Host & devices", "Review pairing and host services", "configuration.read"],
        ["catalog", "Library", "Manage applications in your library", "catalog.read"],
        ["sessions", "Sessions", "View sessions and host metrics", "session.read"],
        ["settings", "Settings", "Configure your host", "configuration.read"],
      ] as const) {
        if (!context.api.allows(scope)) continue;
        const action = button(label, () => context.navigate(page), "quick-action");
        action.append(el("small", "muted", detail), el("span", "quick-action-arrow", "→"));
        quickActions.append(action);
      }
      const serviceRows = snapshots.map((snapshot, index) => [
        el(
          "strong",
          "",
          pagesTextMessage(["Catalog", "Session coordination", "Applications", "Steam provider"][index] ?? ""),
        ),
        snapshot.status === "fulfilled"
          ? badge(pagesTextMessage("Available"), "good")
          : badge(pagesTextMessage("Unavailable")),
        snapshot.status === "rejected"
          ? messageOf(snapshot.reason)
          : pagesTextMessage("Responded to the latest refresh"),
      ]);
      status.setAttribute("role", "status");
      status.setAttribute("aria-live", "polite");
      status.setAttribute("aria-atomic", "true");
      const details = el("div", "overview-details", panel("Service access", table(["Service", "Status", "Details"], serviceRows)));
      if (quickActions.hasChildNodes()) details.append(panel("Quick actions", quickActions));
      area.replaceChildren(status, metrics, details);

    }
    void load();
  }

  function renderOverviewActivity(context: PageContext, container: HTMLElement): () => void {
    if (!context.api.allows("session.read")) {
      container.append(panel("Stream activity", el("p", "muted", "Stream activity requires session.read.")));
      return () => {};
    }

    const summary = el("strong", "", "Loading host stream activity…");
    summary.setAttribute("role", "status");
    summary.setAttribute("aria-live", "polite");
    summary.setAttribute("aria-atomic", "true");
    const launchStatus = el("p", "muted");
    const observation = el("p", "muted");
    const icon = el("div", "overview-status-icon", "↗");
    icon.setAttribute("aria-hidden", "true");
    container.append(el("section", "overview-activity-panel",
      el("div", "overview-activity-state", icon,
        el("div", "overview-activity-copy", el("p", "overview-status-label", "STREAM ACTIVITY"),
          el("h2", "overview-activity-summary", summary), launchStatus, observation)),
      button("View sessions", () => context.navigate("sessions"))));

    let generation = 0;
    let timer: number | undefined;
    let disposed = false;
    const observer = new MutationObserver(() => {
      if (!container.isConnected) cleanup();
    });
    function cleanup(): void {
      if (disposed) return;
      disposed = true;
      generation++;
      if (timer !== undefined) window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", visibilityChanged);
      observer.disconnect();
    }
    function current(request: number): boolean {
      return !disposed && container.isConnected && document.visibilityState === "visible" && request === generation;
    }
    function visibilityChanged(): void {
      if (!container.isConnected) { cleanup(); return; }
      if (document.visibilityState === "visible") refresh();
      else {
        generation++;
        if (timer !== undefined) window.clearTimeout(timer);
        timer = undefined;
      }
    }
    function refresh(): void {
      if (disposed || !container.isConnected || document.visibilityState !== "visible") return;
      if (timer !== undefined) window.clearTimeout(timer);
      timer = undefined;
      const request = ++generation;
      launchStatus.textContent = "Refreshing pending launch tracking…";
      void context.api.post<unknown>("live-streams/status").then((reply) => {
        const value = reply as {ok?: boolean; sessions?: unknown[]; sampled_at_unix_ms?: string};
        if (!value || value.ok !== true || !Array.isArray(value.sessions) || value.sessions.length > 128 || !/^[0-9]+$/.test(value.sampled_at_unix_ms ?? "") || !Number.isSafeInteger(Number(value.sampled_at_unix_ms))) throw new Error("Invalid stream status.");
        const snapshot = {streams:value.sessions, sampledAtUnixMs:Number(value.sampled_at_unix_ms)};
        if (!current(request)) return;
        const count = snapshot.streams.length;
        summary.textContent = count === 0 ? "No active streams reported" :
          `${count} active ${count === 1 ? "stream" : "streams"} reported`;
        observation.textContent = `Host observation: ${new Date(snapshot.sampledAtUnixMs).toLocaleString()}. Client media delivery is not verified.`;
      }).catch(() => {
        if (!current(request)) return;
        summary.textContent = "Live stream status is unavailable";
        observation.textContent = "The host did not provide a current stream status. Try refreshing.";
      }).finally(() => {
        if (current(request)) timer = window.setTimeout(refresh, 10_000);
      });
      void context.api.post<unknown>("history/history_status").then((reply) => {
        const status = parseHistoryRuntimeStatus(reply);
        if (!current(request)) return;
        if (!status.available || !status.configuration_ready) {
          launchStatus.textContent = "Pending launch tracking is unavailable.";
          return;
        }
        launchStatus.textContent = `${status.pending_activities} pending ${status.pending_activities === 1 ? "launch" : "launches"}.${status.degraded ? " Launch tracking is degraded; counts may be incomplete." : ""}`;
      }).catch(() => {
        if (current(request)) launchStatus.textContent = "Pending launch tracking is unavailable.";
      });
    }
    document.addEventListener("visibilitychange", visibilityChanged);
    observer.observe(document.body, { childList: true, subtree: true });
    refresh();
    return refresh;
  }

  export function parseHistoryRuntimeStatus(value: unknown): HistoryRuntimeStatus {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("History status reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !reply.status || typeof reply.status !== "object" || Array.isArray(reply.status))
      throw new Error("History status reply is invalid.");
    const status = reply.status as Record<string, unknown>;
    if (status.applied_revision !== null &&
        (!Number.isSafeInteger(status.applied_revision) || (status.applied_revision as number) < 0))
      throw new Error("History applied revision is invalid.");
    const prune = status.last_prune;
    if (!prune || typeof prune !== "object" || Array.isArray(prune))
      throw new Error("History retention status is invalid.");
    const retention = prune as Record<string, unknown>;
    for (const key of ["available", "configuration_ready", "enabled", "effective_enabled", "degraded"])
      if (typeof status[key] !== "boolean") throw new Error(`History ${key} status is invalid.`);
    for (const key of ["failed_operations", "active_streams", "sessions", "samples", "events", "live_bytes"])
      if (!Number.isSafeInteger(status[key]) || (status[key] as number) < 0)
        throw new Error(`History ${key} count is invalid.`);
    if (status.pending_activities !== undefined &&
        (!Number.isSafeInteger(status.pending_activities) || (status.pending_activities as number) < 0))
      throw new Error("History pending activities count is invalid.");
    for (const key of ["sessions_deleted", "live_bytes", "size_limit_bytes"])
      if (!Number.isSafeInteger(retention[key]) || (retention[key] as number) < 0)
        throw new Error(`History retention ${key} count is invalid.`);
    if (typeof retention.size_limit_unmet !== "boolean")
      throw new Error("History retention limit status is invalid.");
    return { ...status, pending_activities: status.pending_activities ?? 0 } as unknown as HistoryRuntimeStatus;
  }

  export function historyRecordingPolicyDescription(status: HistoryRuntimeStatus): string {
    if (!status.configuration_ready)
      return "History configuration is not ready; recording is not confirmed.";
    if (status.effective_enabled !== status.enabled) {
      const action = status.enabled ? "enable" : "disable";
      const effective = status.effective_enabled ? "enabled" : "disabled";
      if (status.pending_activities > 0) {
        const pending = status.pending_activities === 1
          ? "1 pending launch resolves"
          : `${status.pending_activities} pending launches resolve`;
        const active = status.active_streams > 0
          ? `${status.active_streams} active ${status.active_streams === 1 ? "stream ends" : "streams end"} and `
          : "";
        return `Recording ${action} is deferred until ${active}${pending}; the effective policy remains ${effective}.`;
      }
      return `Recording ${action} is deferred until active streams end; new streams still use the ${effective} policy.`;
    }
    const effective = status.effective_enabled ? "enabled" : "disabled";
    return status.pending_activities > 0
      ? `Recording is ${effective} under the current policy; pending launches have not joined yet.`
      : `Recording is ${effective} for new streams.`;
  }

  export function historyActivityDescription(status: HistoryRuntimeStatus): string {
    const active = `${status.active_streams} active ${status.active_streams === 1 ? "stream" : "streams"}`;
    const pending = `${status.pending_activities} pending ${status.pending_activities === 1 ? "launch" : "launches"}`;
    return `${active} · ${pending}`;
  }

  export function historyRuntimeDescription(status: HistoryRuntimeStatus): string[] {
    if (!status.available)
      return ["History storage is unavailable; recording and recorded session reads cannot run."];
    const lines = [status.applied_revision === null
      ? "Runtime configuration revision is not confirmed."
      : `Runtime applied configuration revision ${status.applied_revision}.`,
      historyRecordingPolicyDescription(status)];
    lines.push(`${historyActivityDescription(status)} · ${status.sessions} stored sessions · ${status.samples} samples · ${status.events} events · ${status.live_bytes} live bytes.`);
    if (status.configuration_ready && !status.effective_enabled)
      lines.push("Stored sessions remain readable. Automatic retention and deletion are paused until recording is enabled.");
    lines.push(`Last retention pass deleted ${status.last_prune.sessions_deleted} sessions; ${status.last_prune.live_bytes} live bytes against ${status.last_prune.size_limit_bytes === 0 ? "no size limit" : `${status.last_prune.size_limit_bytes} byte limit`}.`);
    if (status.last_prune.size_limit_unmet)
      lines.push("The retention size limit remains unmet; active sessions are preserved.");
    if (status.failed_operations > 0)
      lines.push(`${status.failed_operations} history storage operations have failed since this service started.`);
    if (status.degraded && status.failed_operations === 0 && !status.last_prune.size_limit_unmet && status.configuration_ready)
      lines.push("History reports degraded operation.");
    return lines;
  }

  export function parseHistoryDetailReply(value: unknown, sessionId: string): HistoryDetailReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("History detail reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !reply.detail || typeof reply.detail !== "object" || Array.isArray(reply.detail))
      throw new Error("History detail reply is invalid.");
    const detail = reply.detail as Record<string, unknown>;
    const session = detail.session;
    if (!session || typeof session !== "object" || Array.isArray(session) ||
      (session as Record<string, unknown>).session_id !== sessionId)
      throw new Error("History detail belongs to a different session.");
    parseHistoryListReply({ ok: true, sessions: [session], next_offset: null }, 0);
    const summary = session as Record<string, unknown>;
    if (["device_name", "codec"].some((key) => typeof summary[key] !== "string") ||
      ["width", "height", "target_fps", "target_bitrate_kbps"].some((key) =>
        !Number.isSafeInteger(summary[key]) || (summary[key] as number) < 0 || (summary[key] as number) > 0xffff_ffff))
      throw new Error("History session metadata is invalid.");
    for (const key of ["sample_count", "event_count"])
      if (!Number.isSafeInteger(detail[key]) || (detail[key] as number) < 0)
        throw new Error(`History ${key} is invalid.`);
    for (const key of ["samples_truncated", "events_truncated"])
      if (typeof detail[key] !== "boolean") throw new Error(`History ${key} is invalid.`);
    for (const [key, countKey] of [["samples", "sample_count"], ["events", "event_count"]] as const) {
      const rows = detail[key];
      const truncated = detail[key === "samples" ? "samples_truncated" : "events_truncated"];
      if (!Array.isArray(rows) || rows.length > (key === "samples" ? 32 : 8) ||
        rows.length > (detail[countKey] as number) || truncated !== (rows.length < (detail[countKey] as number)) || rows.some((row) =>
        !row || typeof row !== "object" || Array.isArray(row) || row.session_id !== sessionId ||
        !Number.isSafeInteger(row.recorded_at_unix)))
        throw new Error(`History ${key} are invalid.`);
    }
    for (const sample of detail.samples as Record<string, unknown>[]) {
      for (const key of ["frames_sent", "bytes_sent", "dropped_frames"])
        if (!Number.isSafeInteger(sample[key]) || (sample[key] as number) < 0)
          throw new Error(`History sample ${key} is invalid.`);
      for (const key of ["actual_fps", "actual_bitrate_kbps"])
        if (typeof sample[key] !== "number" || !Number.isFinite(sample[key]) || sample[key] < 0)
          throw new Error(`History sample ${key} is invalid.`);
      if (sample.encode_latency_ms != null && (typeof sample.encode_latency_ms !== "number" ||
        !Number.isFinite(sample.encode_latency_ms) || sample.encode_latency_ms < 0))
        throw new Error("History sample encode_latency_ms is invalid.");
    }
    if ((detail.events as Record<string, unknown>[]).some((event) => typeof event.event_type !== "string"))
      throw new Error("History event type is invalid.");
    return reply as unknown as HistoryDetailReply;
  }

  export function parseHistoryListReply(value: unknown, offset: number): HistoryListReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("History session page is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Array.isArray(reply.sessions) || reply.sessions.length > 25 ||
        (reply.next_offset !== null && (!Number.isSafeInteger(reply.next_offset) ||
          (reply.next_offset as number) <= offset)))
      throw new Error("History session page is invalid.");
    const ids = new Set<string>();
    for (const entry of reply.sessions) {
      if (!entry || typeof entry !== "object" || Array.isArray(entry))
        throw new Error("History session row is invalid.");
      const session = entry as Record<string, unknown>;
      if (typeof session.session_id !== "string" || session.session_id.length === 0 ||
          new TextEncoder().encode(session.session_id).length > 128 ||
          /[\u0000-\u001f\u007f-\u009f]/u.test(session.session_id) ||
          ["app_name", "client_name", "device_name", "protocol", "codec"].some((key) => typeof session[key] !== "string") ||
          ["width", "height", "target_fps", "target_bitrate_kbps"].some((key) =>
            !Number.isSafeInteger(session[key]) || (session[key] as number) < 0 || (session[key] as number) > 0xffff_ffff) ||
          !Number.isSafeInteger(session.started_at_unix) ||
          (session.ended_at_unix !== null && !Number.isSafeInteger(session.ended_at_unix)) ||
          (session.duration_seconds !== null &&
            (!Number.isSafeInteger(session.duration_seconds) || (session.duration_seconds as number) < 0)))
        throw new Error("History session row is invalid.");
      if (ids.has(session.session_id)) throw new Error("History session page contains a duplicate stream.");
      ids.add(session.session_id);
    }
    return reply as unknown as HistoryListReply;
  }

  export function parseHistoryDeleteReply(value: unknown): void {
    if (!value || typeof value !== "object" || Array.isArray(value) ||
        (value as Record<string, unknown>).ok !== true)
      throw new Error("History deletion was not confirmed. Refresh the recorded sessions before trying again.");
  }

  export async function confirmHistoryDeletion(api: Pick<Api, "post">, sessionId: string): Promise<void> {
    try {
      await api.post<unknown>("history/session_detail", { session_id: sessionId });
    } catch (error) {
      if (error && typeof error === "object" &&
          (error as { code?: unknown }).code === "not_found") return;
      throw error;
    }
    throw new Error("History deletion could not be confirmed by readback. Refresh the recorded sessions.");
  }

  export function renderSessions(context: PageContext): void {
    const controls = el("div");
    context.content.append(sectionHeading("Sessions", "Manage the current streaming session."), controls);
    Amaterasu.renderManagedStreamControls(context, controls);
  }

  export function renderApplications(context: PageContext): void {
    const area = el("div");
    const status = el("div");
    const launchesArea = el("div");
    let offset = 0;
    const history = [0];
    context.content.append(
      sectionHeading(
        "Applications",
        "Processes launched through this management console.",
        button("Refresh", () => { void load(); void loadLaunches(); }),
      ),
      status,
      panel("Managed launches", launchesArea),
      area,
    );
    async function loadLaunches(): Promise<void> {
      try {
        const reply = await context.api.post<{ launches: { launch_id: number; app_id: string; state: string }[] }>("launch/list", { limit: 16 });
        if (!context.content.isConnected) return;
        if (!Array.isArray(reply.launches)) throw new Error("Invalid launch list.");
        const rows = reply.launches.map((launch) => {
          if (!Number.isSafeInteger(launch.launch_id) || launch.launch_id < 1 || typeof launch.app_id !== "string" || typeof launch.state !== "string") throw new Error("Invalid launch list.");
          const stop = button("Stop", () => void runAction(stop, status, async () => {
            if (!(await confirmAction("Stop this launch?", "The managed application and its preparation will be stopped.", "Stop"))) return;
            await context.api.post("launch/stop", { launch_id: launch.launch_id });
            context.notify("Stop requested. Refresh to check its status.");
            await loadLaunches();
          }));
          stop.disabled = ["stopped", "failed"].includes(launch.state) || typeof context.api.allowsLaunchAction !== "function" || !context.api.allowsLaunchAction("stop");
          return [pagesTextLiteral(launch.app_id), pagesTextLiteral(launch.state), stop];
        });
        launchesArea.replaceChildren(rows.length ? table(["Application", "State", "Action"], rows) : empty("No managed launches", "Applications started from the library appear here."));
      } catch (error) { if (context.content.isConnected) launchesArea.replaceChildren(errorPanel(error, loadLaunches)); }
    }
    async function load(): Promise<void> {
      try {
        const reply = await context.api.post<ProcessReply>("application/list", {
          offset,
          limit: 16,
        });
        if (!context.content.isConnected) return;
        const rows = reply.processes.map((process) => {
          const terminate = button(
            "Terminate",
            async () => {
              if (
                !(await confirmAction(
                  "Terminate this application?",
                  pagesTextLiteral(`Stop process ${process.pid ?? process.process_id} and its owned process tree. Unsaved work may be lost.`),
                  "Terminate",
                ))
              )
                return;
              await runAction(terminate, status, async () => {
                await context.api.post("application/terminate", {
                  process_id: process.process_id,
                });
                context.notify(pagesTextMessage("Termination requested. Refresh to check its status."));
                await load();
              });
            },
            "quiet danger-text",
          );
          terminate.disabled =
            process.state !== "running" ||
            !context.api.allows("application.terminate");
          return [
            el("strong", "", pagesTextLiteral(process.operation_id)),
            String(process.pid ?? "—"),
            badge(pagesTextLiteral(process.state), process.state === "running" ? "good" : ""),
            process.exit_code != null ? String(process.exit_code) : "—",
            terminate,
          ];
        });
        area.replaceChildren(
          reply.processes.length
            ? table(
                [
                  "Application operation",
                  "PID",
                  "State",
                  "Exit code",
                  "Action",
                ],
                rows,
              )
            : empty(
                "No tracked processes",
                "Applications launched through the management API appear here. Catalog entries are metadata and do not launch automatically.",
              ),
          pager(
            history.length > 1
              ? () => {
                  history.pop();
                  offset = history[history.length - 1] ?? 0;
                  void load();
                }
              : null,
            reply.next_offset != null
              ? () => {
                  offset = reply.next_offset!;
                  history.push(offset);
                  void load();
                }
              : null,
            pagesTextLiteral(`Showing from ${offset + 1}`),
          ),
        );
      } catch (error) {
        if (context.content.isConnected)
          area.replaceChildren(errorPanel(error, load));
      }
    }
    void load();
    void loadLaunches();
  }

  interface SteamCatalogSyncStatus {
    enabled: boolean;
    syncing: boolean;
    force_sync_pending: boolean;
    last_error: string | null;
  }

  interface SteamForceSyncStatus extends SteamStatus {
    settings_ready: boolean;
    force_sync_pending: boolean;
    catalog_sync: SteamCatalogSyncStatus;
  }

  interface SteamForceSyncReply {
    ok?: boolean;
    accepted?: boolean;
    settings_ready?: boolean;
    force_sync_pending?: boolean;
    catalog_sync?: Partial<SteamCatalogSyncStatus>;
    code?: string;
  }

  export function steamForceSyncOutcome(
    provider: SteamForceSyncStatus,
  ): "pending" | "success" | "failure" {
    if (provider.enabled === false) return "failure";
    if (
      provider.force_sync_pending !== false ||
      provider.catalog_sync?.force_sync_pending !== false
    ) {
      return "pending";
    }
    return provider.complete && provider.catalog_sync?.last_error === null
      ? "success"
      : "failure";
  }

  export function renderSteam(context: PageContext): void {
    const area = el("div");
    const status = el("div");
    let revision: number | undefined;
    let after: number | undefined;
    let providerStatus: SteamForceSyncStatus | undefined;
    let forceSyncPending = false;
    let forceSyncPollTimer: number | undefined;
    let forceSyncPollGeneration = 0;
    let forceSyncControl: HTMLButtonElement | undefined;
    const history: (number | undefined)[] = [undefined];

    function resetPagination(): void {
      revision = undefined;
      after = undefined;
      history.splice(0, history.length, undefined);
    }

    const scan = button(
      "Scan libraries",
      () =>
        runAction(scan, status, async () => {
          await context.api.post("steam/refresh");
          resetPagination();
          context.notify(pagesTextMessage("Steam scan requested. Refresh to see completed discoveries."));
          await load();
        }),
      "primary",
    );

    function updateForceSyncControl(): void {
      if (!forceSyncControl) return;
      const allowed = context.api.allows("steam.refresh");
      forceSyncControl.hidden = !allowed;
      forceSyncControl.disabled =
        !allowed ||
        providerStatus?.enabled === false ||
        providerStatus?.settings_ready === false ||
        forceSyncPending;
    }

    function stopForceSyncPolling(): void {
      forceSyncPollGeneration += 1;
      if (forceSyncPollTimer !== undefined) {
        window.clearTimeout(forceSyncPollTimer);
        forceSyncPollTimer = undefined;
      }
    }

    function stopUnobservableForceSync(): void {
      if (!forceSyncPending) return;
      forceSyncPending = false;
      stopForceSyncPolling();
      updateForceSyncControl();
      const message =
        "Force catalog sync was accepted, but this session lacks steam.read and cannot check its status.";
      status.replaceChildren(el("p", "notice warning", pagesTextLiteral(message)));
      context.notify(message);
    }

    function finishForceSync(provider: SteamForceSyncStatus): void {
      const outcome = steamForceSyncOutcome(provider);
      if (outcome === "pending") return;
      forceSyncPending = false;
      stopForceSyncPolling();
      updateForceSyncControl();
      if (outcome === "success") {
        status.replaceChildren(
          el("p", "notice good", "Steam discovery completed and catalog sync succeeded."),
        );
        context.notify(pagesTextMessage("Steam discovery completed and catalog sync succeeded."));
        return;
      }

      const reason = provider.catalog_sync?.last_error;
      const message = reason
        ? `Steam catalog sync did not complete: ${reason}`
        : provider.enabled === false
          ? "Steam discovery was disabled; no catalog sync was confirmed."
          : provider.complete
            ? "Steam catalog sync ended without a confirmed successful result."
            : "Steam discovery was incomplete; no catalog sync was confirmed.";
      status.replaceChildren(el("p", "notice warning", pagesTextLiteral(message)));
      context.notify(message);
    }

    async function pollForceSync(generation: number): Promise<void> {
      if (!context.content.isConnected) {
        stopForceSyncPolling();
        return;
      }
      if (!context.api.allows("steam.read")) {
        stopUnobservableForceSync();
        return;
      }
      if (
        generation !== forceSyncPollGeneration ||
        !forceSyncPending ||
        !context.content.isConnected
      ) {
        return;
      }
      try {
        const provider = await context.api.post<SteamForceSyncStatus>(
          "steam/status",
        );
        if (
          generation !== forceSyncPollGeneration ||
          !forceSyncPending ||
          !context.content.isConnected
        ) {
          return;
        }
        providerStatus = provider;
        finishForceSync(provider);
        updateForceSyncControl();
        if (!forceSyncPending && context.content.isConnected) {
          resetPagination();
          await load(false);
        }
      } catch (error) {
        if (
          generation !== forceSyncPollGeneration ||
          !forceSyncPending ||
          !context.content.isConnected
        ) {
          return;
        }
        if (!context.api.allows("steam.read")) {
          stopUnobservableForceSync();
          return;
        }
        status.replaceChildren(errorPanel(error));
      }

      if (
        generation === forceSyncPollGeneration &&
        forceSyncPending &&
        context.content.isConnected
      ) {
        forceSyncPollTimer = window.setTimeout(() => {
          forceSyncPollTimer = undefined;
          void pollForceSync(generation);
        }, 1000);
      }
    }

    function startForceSyncPolling(): void {
      if (!forceSyncPending || !context.content.isConnected) return;
      if (!context.api.allows("steam.read")) {
        stopUnobservableForceSync();
        return;
      }
      stopForceSyncPolling();
      const generation = forceSyncPollGeneration;
      forceSyncPollTimer = window.setTimeout(() => {
        forceSyncPollTimer = undefined;
        void pollForceSync(generation);
      }, 1000);
    }

    async function requestForceSync(): Promise<void> {
      const target = forceSyncControl;
      if (!target) return;
      await runAction(target, status, async () => {
        const reply = await context.api.post<SteamForceSyncReply>(
          "steam/force_sync",
        );
        if (
          reply.ok !== true ||
          reply.accepted !== true ||
          reply.settings_ready !== true ||
          reply.force_sync_pending !== true ||
          reply.catalog_sync?.force_sync_pending !== true
        ) {
          throw new Error(
            reply.code
              ? `Force catalog sync was not accepted: ${reply.code}`
              : "Force catalog sync was not accepted.",
          );
        }

        forceSyncPending = true;
        status.replaceChildren(
          el(
            "p",
            "notice",
            "Force catalog sync accepted. Waiting for a complete Steam scan.",
          ),
        );
        updateForceSyncControl();
        if (!context.content.isConnected) return;
        if (!context.api.allows("steam.read")) {
          stopUnobservableForceSync();
          return;
        }
        resetPagination();
        await load();
        startForceSyncPolling();
      });
      updateForceSyncControl();
    }

    const canForceSync = context.api.allows("steam.refresh");
    if (canForceSync) {
      forceSyncControl = button(
        "Force catalog sync",
        requestForceSync,
      );
    }
    updateForceSyncControl();
    scan.disabled = !context.api.allows("steam.refresh");
    context.content.append(
      sectionHeading(
        "Steam library",
        "Discover local Steam games and optionally sync a complete scan into the catalog.",
        button("Refresh", () => {
          resetPagination();
          return load();
        }),
        scan,
        forceSyncControl,
      ),
      status,
      area,
    );
    async function load(refreshProviderStatus = true): Promise<void> {
      if (!context.api.allows("steam.read")) {
        if (context.content.isConnected) {
          area.replaceChildren(
            empty(
              "Steam status unavailable",
              "This session can request a Steam force sync but needs steam.read to inspect discoveries or track its progress.",
            ),
          );
        }
        return;
      }
      try {
        const providerRequest = refreshProviderStatus
          ? context.api.post<SteamForceSyncStatus>("steam/status")
          : Promise.resolve(providerStatus);
        const [reply, provider] = await Promise.all([
          context.api.post<SteamReply>("steam/list", {
            ...(revision != null ? { revision } : {}),
            ...(after != null ? { after } : {}),
            limit: 16,
          }),
          providerRequest,
        ]);
        if (!context.content.isConnected || !provider) return;
        revision = reply.revision;
        providerStatus = provider;
        scan.disabled =
          !context.api.allows("steam.refresh") || provider.enabled === false;
        if (forceSyncPending) finishForceSync(provider);
        updateForceSyncControl();
        const rows = reply.games.map((game) => [
          el(
            "div",
            "",
            el("strong", "", pagesTextLiteral(game.name)),
            el("small", "path block", pagesTextLiteral(game.install_directory ?? "")),
          ),
          String(game.app_id),
          badge(pagesTextMessage(game.installed ? "Installed" : "Discovered")),
        ]);
        area.replaceChildren(
          el(
            "div",
            "section-meta",
            badge(
              pagesTextMessage(provider.enabled === false
                ? "Discovery disabled"
                : provider.scanning
                ? "Scan in progress"
                : provider.complete
                  ? "Scan complete"
                  : "Scan incomplete"),
            ),
            el(
              "span",
              "muted",
              pagesTextLiteral(`${provider.game_count} discoveries · revision ${reply.revision}`),
            ),
          ),
          reply.games.length
            ? table(["Application", "Steam app ID", "Status"], rows)
            : empty(
                "No Steam discoveries",
                provider.enabled === false
                  ? "Enable Steam discovery in Settings to scan local libraries."
                  : "Configure Steam library roots on the host, then scan. Discovery does not import or launch games.",
              ),
          ...(provider.warnings ?? []).map((warning) =>
            el("p", "notice", pagesTextLiteral(warning)),
          ),
          pager(
            history.length > 1
              ? () => {
                  history.pop();
                  after = history[history.length - 1];
                  void load();
                }
              : null,
            reply.next_after != null
              ? () => {
                  after = reply.next_after!;
                  history.push(after);
                  void load();
                }
              : null,
            pagesTextLiteral(`Page ${history.length}`),
          ),
        );
      } catch (error) {
        if (context.content.isConnected)
          area.replaceChildren(
            errorPanel(error, () => {
              revision = undefined;
              after = undefined;
              history.splice(0, history.length, undefined);
              return load();
            }),
          );
      }
    }
    void load();
  }

  export function parseDiagnosticQueryReply(value: unknown, after: number): DiagnosticReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Diagnostics query reply is invalid.");
    const reply = value as Record<string, unknown>;
    const nonnegative = (number: unknown): number is number =>
      typeof number === "number" && Number.isSafeInteger(number) && number >= 0;
    const validName = (name: unknown): name is string =>
      typeof name === "string" && /^[A-Za-z0-9._\-/]{1,64}$/.test(name);
    if (reply.ok !== true || !Array.isArray(reply.records) || reply.records.length > 4 ||
        typeof reply.has_more !== "boolean" || typeof reply.resync_required !== "boolean" ||
        !nonnegative(reply.next_cursor) || !nonnegative(reply.latest_sequence) ||
        !nonnegative(reply.dropped_total) ||
        (reply.oldest_sequence !== null && !nonnegative(reply.oldest_sequence)) ||
        reply.next_cursor < after || reply.next_cursor > reply.latest_sequence)
      throw new Error("Diagnostics query reply is invalid or exceeds exact browser integer range.");
    let sequence = after;
    for (const entry of reply.records) {
      if (!entry || typeof entry !== "object" || Array.isArray(entry))
        throw new Error("Diagnostics record is invalid.");
      const record = entry as Record<string, unknown>;
      if (!nonnegative(record.sequence) || record.sequence <= sequence ||
          !["trace", "debug", "info", "warn", "error"].includes(record.level as string) ||
          !validName(record.target) || typeof record.message !== "string" ||
          new TextEncoder().encode(record.message).length > 512 ||
          !record.fields || typeof record.fields !== "object" || Array.isArray(record.fields) ||
          Object.keys(record.fields).length > 8 ||
          Object.entries(record.fields).some(([name, field]) => !validName(name) ||
            typeof field !== "string" || new TextEncoder().encode(field).length > 256))
        throw new Error("Diagnostics record is invalid.");
      sequence = record.sequence;
    }
    if (reply.next_cursor !== sequence || reply.has_more !== (sequence < reply.latest_sequence))
      throw new Error("Diagnostics query cursor is inconsistent.");
    return reply as unknown as DiagnosticReply;
  }

  export interface DiagnosticSnapshot {
    records: DiagnosticRecord[];
    latestSequence: number;
    droppedTotal: number;
    incomplete: boolean;
  }

  // The journal is bounded to 128 records and the API permits four per request.
  // Freeze the first reply's endpoint so new writes cannot extend the read forever.
  export async function readDiagnosticSnapshot(
    api: Pick<Api, "post">,
    isCurrent: () => boolean,
  ): Promise<DiagnosticSnapshot | null> {
    let cursor = 0;
    let target: number | undefined;
    let droppedTotal = 0;
    let incomplete = false;
    const records: DiagnosticRecord[] = [];
    for (let page = 0; page < 32; page++) {
      const value = await api.post<unknown>("diagnostics/query", { after: cursor, limit: 4 });
      if (!isCurrent()) return null;
      const reply = parseDiagnosticQueryReply(value, cursor);
      if (target === undefined) {
        target = reply.latest_sequence;
        droppedTotal = reply.dropped_total;
      } else if (reply.resync_required) {
        incomplete = true;
      }
      const expectedFirst = page === 0 ? (reply.oldest_sequence ?? 1) : cursor + 1;
      let expected = expectedFirst;
      for (const record of reply.records) {
        if (record.sequence !== expected) incomplete = true;
        expected = record.sequence + 1;
        if (record.sequence <= target) records.push(record);
      }
      if (reply.next_cursor >= target) return { records, latestSequence: target, droppedTotal, incomplete };
      if (reply.next_cursor <= cursor) throw new Error("Diagnostics query did not advance through the journal.");
      cursor = reply.next_cursor;
    }
    return { records, latestSequence: target ?? 0, droppedTotal, incomplete: true };
  }

  export function formatDiagnosticRecords(records: DiagnosticRecord[]): string {
    return records.map((record) => {
      const fields = Object.entries(record.fields).map(([key, value]) => `${key}: ${value}`).join(" · ");
      return `#${record.sequence} [${record.level}] ${record.target}: ${record.message}${fields ? ` · ${fields}` : ""}`;
    }).join("\n");
  }

  export function parseDiagnosticCountersReply(value: unknown): Record<string, number> {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Diagnostics counters reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !reply.counters || typeof reply.counters !== "object" ||
        Array.isArray(reply.counters))
      throw new Error("Diagnostics counters reply is invalid.");
    const counters = reply.counters as Record<string, unknown>;
    if (Object.entries(counters).some(([name, count]) =>
      !/^[A-Za-z0-9._\-/]{1,64}$/.test(name) ||
      typeof count !== "number" || !Number.isSafeInteger(count) || count < 0))
      throw new Error("Diagnostics counter value is invalid or exceeds exact browser integer range.");
    return counters as Record<string, number>;
  }

  export function filterDiagnosticPage(records: DiagnosticRecord[], query: string, level: string): DiagnosticRecord[] {
    const needle = query.trim().toLocaleLowerCase();
    return records.filter((record) => {
      if (level && record.level !== level) return false;
      const text = [record.target, record.message, ...Object.entries(record.fields).flat()]
        .join(" ").toLocaleLowerCase();
      return !needle || text.includes(needle);
    });
  }

  const DIAGNOSTIC_SESSION_LIMIT = 4;

  function diagnosticFailureStage(reason: unknown): string {
    if (typeof reason !== "string") return "Failure · stage not identified";
    const stages: Record<string, string> = {
      capture_failed: "Capture",
      capture_read_failed: "Capture",
      capture_geometry_failed: "Capture",
      drm_capture_failed: "Capture",
      encoder_failed: "Video encoder",
      encoder_drain_failed: "Video encoder",
      software_encoder_failed: "Video encoder",
      video_failed_after_admission: "Video pipeline · stage not identified",
      audio_encoder_failed: "Audio encoder",
      audio_capture_failed: "Audio capture",
      audio_backend_failed: "Audio capture",
      audio_packetization_failed: "Audio transport",
      transport_drain_failed: "RTP / media transport",
      udp_send_failed: "RTP / media transport",
      udp_stalled: "RTP / media transport",
      control_failed: "Control channel",
      control_transport_failed: "Control channel",
      peer_disconnected: "Control channel",
      authenticated_peer_idle: "Control channel",
      authentication_timeout: "Control channel",
    };
    return Object.hasOwn(stages, reason) ? `Failure · ${stages[reason]}` : "Failure · stage not identified";
  }

  export function diagnosticSessionOutcome(detail: HistoryDetailReply["detail"]): string {
    if (!detail) return "Outcome unavailable";
    const events = detail.events
      .filter((event) => ["stream_stopping", "stream_stopped", "stream_cancelled", "stream_failed"].includes(event.event_type))
      .sort((left, right) => right.recorded_at_unix - left.recorded_at_unix);
    const terminal = events.find((event) => event.event_type !== "stream_stopping") ?? events[0];
    if (!terminal) {
      if (detail.events_truncated) return "Outcome unavailable · event history is truncated";
      return detail.session.ended_at_unix == null
        ? "Active · no termination event"
        : "Ended · no termination event recorded";
    }
    if (terminal.event_type === "stream_stopped") return "Stopped";
    if (terminal.event_type === "stream_cancelled") return "Cancelled";
    const payload = terminal.payload;
    const reason = payload && typeof payload === "object" && !Array.isArray(payload)
      ? (payload as Record<string, unknown>).reason
      : undefined;
    if (terminal.event_type === "stream_stopping")
      return reason == null ? "Stopping · termination not confirmed" : `Stopping · ${diagnosticFailureStage(reason)}`;
    return diagnosticFailureStage(reason);
  }

  export function renderDiagnostics(context: PageContext): void {
    const area = el("div");
    const performanceArea = el("div");
    const hostLogArea = el("div");
    const sessionsArea = el("div");
    let loadRequest = 0;
    let sessionRequest = 0;
    let refreshHostLogs: (() => Promise<void>) | null = null;
    let refreshPerformance: (() => Promise<void>) | null = null;
    let currentRecords: DiagnosticRecord[] = [];
    const filter = input("diagnostics_filter", "", "search");
    filter.placeholder = "Search retained records";
    const level = el("select");
    for (const [value, label] of [["", "All levels"], ["trace", "Trace"], ["debug", "Debug"],
      ["info", "Info"], ["warn", "Warning"], ["error", "Error"]]) {
      const option = el("option", "", label);
      option.value = value ?? "";
      level.append(option);
    }
    const filteredRecords = el("div", "records");
    const filterSummary = el("p", "muted diagnostics-filter-summary");
    filterSummary.setAttribute("role", "status");
    const copyStatus = el("div");
    const matchStatus = el("span", "muted diagnostics-match-status");
    matchStatus.setAttribute("role", "status");
    let selectedMatch = 0;
    const previousMatch = button("Previous match", () => moveMatch(-1));
    const nextMatch = button("Next match", () => moveMatch(1));
    const matchNavigation = el("div", "diagnostics-match-navigation", previousMatch, nextMatch, matchStatus);
    matchNavigation.setAttribute("role", "group");
    matchNavigation.setAttribute("aria-label", "Search match navigation");
    let copying = false;
    const copy = button("Copy visible records", () => {
      if (copying) return;
      copying = true;
      void runAction(copy, copyStatus, async () => {
        const records = filterDiagnosticPage(currentRecords, filter.value, level.value);
        if (!navigator.clipboard?.writeText) throw new Error("Clipboard access is unavailable in this browser context.");
        await navigator.clipboard.writeText(formatDiagnosticRecords(records));
        context.notify("Visible diagnostic records copied.");
      }).finally(() => {
        copying = false;
        copy.disabled = filterDiagnosticPage(currentRecords, filter.value, level.value).length === 0;
      });
    });
    const download = button("Download visible records", () => {
      const records = filterDiagnosticPage(currentRecords, filter.value, level.value);
      if (!records.length) return;
      const url = URL.createObjectURL(new Blob([formatDiagnosticRecords(records)], { type: "text/plain;charset=utf-8" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = "amaterasu-diagnostics.txt";
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 0);
    });
    function moveMatch(direction: number): void {
      const visible = filterDiagnosticPage(currentRecords, filter.value, level.value);
      if (!filter.value.trim() || visible.length === 0) return;
      selectedMatch = (selectedMatch + direction + visible.length) % visible.length;
      renderFilteredRecords(true);
    }
    function renderFilteredRecords(scrollToMatch = false): void {
      const visible = filterDiagnosticPage(currentRecords, filter.value, level.value);
      const searching = filter.value.trim().length > 0;
      if (selectedMatch >= visible.length) selectedMatch = 0;
      matchNavigation.hidden = !searching;
      previousMatch.disabled = nextMatch.disabled = visible.length === 0;
      matchStatus.textContent = visible.length ? `Match ${selectedMatch + 1} of ${visible.length}` : "No matches";
      copy.disabled = copying || visible.length === 0;
      download.disabled = visible.length === 0;
      filterSummary.textContent = `${visible.length} of ${currentRecords.length} retained records`;
      const articles = visible.map((record, index) => {
        const active = searching && index === selectedMatch;
        const article = el(
          "article", `record${active ? " diagnostics-record-active" : ""}`,
          el("div", "section-meta",
            badge(pagesTextLiteral(record.level), record.level === "error" ? "bad" : ""),
            el("small", "muted", pagesTextLiteral(`#${record.sequence} · ${record.target}`))),
          el("p", "", pagesTextLiteral(record.message)),
          ...Object.entries(record.fields).map(([key, value]) =>
            el("p", "record-field", el("strong", "", pagesTextLiteral(`${key}: `)), pagesTextLiteral(value))),
        );
        if (active) article.setAttribute("aria-current", "true");
        return article;
      });
      filteredRecords.replaceChildren(...articles);
      if (scrollToMatch && searching && articles[selectedMatch]?.scrollIntoView)
        articles[selectedMatch]!.scrollIntoView({ block: "nearest", behavior: "smooth" });
      if (!visible.length) filteredRecords.append(el("p", "muted diagnostics-empty", currentRecords.length
        ? "No retained records match the filter."
        : "No diagnostic records are retained."));
    }
    filter.addEventListener("input", () => { selectedMatch = 0; renderFilteredRecords(); });
    filter.addEventListener("keydown", (event) => {
      if (event.key !== "Enter" || event.isComposing || !filter.value.trim()) return;
      event.preventDefault();
      moveMatch(event.shiftKey ? -1 : 1);
    });
    level.addEventListener("change", () => { selectedMatch = 0; renderFilteredRecords(); });
    context.content.append(
      sectionHeading(
        "Diagnostics",
        "Stream performance, host logs, caller-scoped records, and separately recorded stream outcomes.",
        button("Refresh", () => {
          void refreshPerformance?.();
          void refreshHostLogs?.();
          void loadRecentSessions();
          return load();
        }),
      ),
      performanceArea,
      hostLogArea,
      area,
      panel("Recent stream outcomes", sessionsArea,
        el("p", "muted", "Session history is separate from caller-scoped diagnostics; no record-to-session correlation is implied. Only recognized termination codes receive a stage label.")),
    );
    async function loadRecentSessions(): Promise<void> {
      const request = ++sessionRequest;
      if (!context.api.allows("session.read")) {
        sessionsArea.replaceChildren(empty(
          "Session history unavailable",
          "This session needs session.read to inspect recent stream outcomes.",
        ));
        return;
      }
      sessionsArea.replaceChildren(el("p", "muted", "Loading recent stream outcomes…"));
      try {
        const list = parseHistoryListReply(await context.api.post<unknown>(
          "history/list_sessions", { limit: DIAGNOSTIC_SESSION_LIMIT, offset: 0 },
        ), 0);
        if (!context.content.isConnected || request !== sessionRequest) return;
        if (!list.sessions.length) {
          sessionsArea.replaceChildren(empty(
            "No recorded streams",
            "Recent outcomes appear when session history is available and recording has captured a stream.",
          ));
          return;
        }
        const details = await Promise.all(list.sessions.map(async (session) => {
          try {
            const detail = parseHistoryDetailReply(await context.api.post<unknown>(
              "history/session_detail", { session_id: session.session_id },
            ), session.session_id).detail;
            return { session, outcome: diagnosticSessionOutcome(detail) };
          } catch {
            return { session, outcome: "Outcome unavailable · refresh to retry" };
          }
        }));
        if (!context.content.isConnected || request !== sessionRequest) return;
        sessionsArea.replaceChildren(table(
          ["Started", "State", "Recorded outcome"],
          details.map(({ session, outcome }) => [
            new Date(session.started_at_unix * 1000).toLocaleString(),
            session.ended_at_unix == null ? badge("Active", "good") : "Ended",
            outcome,
          ]),
        ), el("p", "muted", `Showing at most ${DIAGNOSTIC_SESSION_LIMIT} recent sessions. Client and device identity are not displayed.`));
      } catch (error) {
        if (context.content.isConnected && request === sessionRequest)
          sessionsArea.replaceChildren(errorPanel(error, loadRecentSessions));
      }
    }
    async function load(): Promise<void> {
      const request = ++loadRequest;
      const isCurrent = () => context.content.isConnected && request === loadRequest;
      if (!context.api.allows("diagnostics.read")) {
        area.replaceChildren(panel("Caller-scoped diagnostics", el("p", "muted", "This session does not have diagnostics.read access.")));
        return;
      }
      area.replaceChildren(el("p", "muted", "Loading retained diagnostic records…"));
      try {
        const [snapshotResult, countersResult] = await Promise.allSettled([
          readDiagnosticSnapshot(context.api, isCurrent),
          context.api.post<unknown>("diagnostics/counters"),
        ]);
        if (!isCurrent()) return;
        if (snapshotResult.status === "rejected") throw snapshotResult.reason;
        const snapshot = snapshotResult.value;
        if (!snapshot) return;
        currentRecords = snapshot.records;
        selectedMatch = 0;
        renderFilteredRecords();
        let countersContent: HTMLElement;
        try {
          if (countersResult.status === "rejected") throw countersResult.reason;
          const counters = parseDiagnosticCountersReply(countersResult.value);
          countersContent = Object.keys(counters).length
            ? table(["Counter", "Value"], Object.entries(counters).map(([name, value]) => [name, String(value)]))
            : el("p", "muted", "No counters have been recorded.");
        } catch (error) {
          countersContent = errorPanel(error, load);
        }
        area.replaceChildren(
          snapshot.incomplete
            ? el("p", "notice", "Some records disappeared while this snapshot was loading. Refresh to try again.")
            : nullNode(),
          snapshot.droppedTotal > 0
            ? el("p", "notice", `${snapshot.droppedTotal} older records were removed from the bounded journal before this snapshot.`)
            : nullNode(),
          panel(
            "Recent records",
            el("p", "muted", "Search, copy or download this caller-scoped snapshot of up to 128 retained records. Host log files are not included; refresh to see newer records."),
            el("div", "diagnostics-toolbar", field("Search retained records", filter), field("Level", level), copy, download),
            filterSummary, matchNavigation, copyStatus, filteredRecords,
          ),
          panel("Counters", countersContent),
        );
      } catch (error) {
        if (isCurrent()) area.replaceChildren(errorPanel(error, load));
      }
    }
    if (context.api.allows("diagnostics.read")) refreshPerformance = renderPerformance(context, performanceArea);
    if (context.api.allows("host-logs.read")) refreshHostLogs = renderHostLogs(context, hostLogArea);
    void load();
    void loadRecentSessions();
  }
  function nullNode(): Text {
    return document.createTextNode("");
  }
}
