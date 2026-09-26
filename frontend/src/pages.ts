namespace Amaterasu {
  export function renderOverview(context: PageContext): void {
    const area = el("div");
    context.content.append(
      sectionHeading(
        "Host overview",
        "Your applications, sessions, and local services in one place.",
        button("Refresh", load),
      ),
      area,
    );
    async function load(): Promise<void> {
      const snapshots = await Promise.allSettled([
        context.api.post<CatalogReply>("catalog/list", { limit: 32 }),
        context.api.post<SessionReply>("session/list", { limit: 16 }),
        context.api.post<ProcessReply>("application/list", { limit: 16 }),
        context.api.post<SteamStatus>("steam/status"),
      ]);
      if (!context.content.isConnected) return;
      const [catalog, sessions, applications, steam] = snapshots;
      const metrics = el("div", "metrics");
      const addMetric = (label: string, value: string, detail: string) =>
        metrics.append(
          el(
            "div",
            "metric",
            el("span", "muted", label),
            el("strong", "", value),
            el("small", "muted", detail),
          ),
        );
      addMetric(
        "Catalog applications",
        catalog?.status === "fulfilled"
          ? countLabel(catalog.value.entries.length, catalog.value.next_cursor)
          : "—",
        "Manual library entries",
      );
      addMetric(
        "Sessions",
        sessions?.status === "fulfilled"
          ? countLabel(
              sessions.value.sessions.length,
              sessions.value.next_offset,
            )
          : "—",
        "Current host instance",
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
      const hero = el(
        "section",
        "overview-hero",
        el(
          "div",
          "",
          badge("YOUR HOST, YOUR LIBRARY", "accent"),
          el("h2", "", "A home for your applications."),
          el(
            "p",
            "",
            "Curate your library and manage the services that keep this host running.",
          ),
          button(
            "Open your library →",
            () => context.navigate("catalog"),
            "primary",
          ),
        ),
        el(
          "div",
          "orbit-art",
          el("span", "orbit one"),
          el("span", "orbit two"),
          el("span", "orbit-core", "A"),
        ),
      );
      const serviceRows = snapshots.map((snapshot, index) => [
        el(
          "strong",
          "",
          ["Catalog", "Sessions", "Applications", "Steam provider"][index] ??
            "",
        ),
        snapshot.status === "fulfilled"
          ? badge("Available", "good")
          : badge("Unavailable"),
        snapshot.status === "rejected"
          ? messageOf(snapshot.reason)
          : "Responded to the latest refresh",
      ]);
      area.replaceChildren(
        hero,
        metrics,
        panel(
          "Service access",
          table(["Service", "Status", "Details"], serviceRows),
        ),
        el(
          "div",
          "notice",
          el("strong", "", "Streaming is still being built"),
          el(
            "p",
            "",
            "Client pairing, capture, and stream connections are not available in this release. The management tools shown here are live.",
          ),
        ),
      );
    }
    void load();
  }

  export function parseHistoryRuntimeStatus(value: unknown): HistoryRuntimeStatus {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("History status reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !reply.status || typeof reply.status !== "object" || Array.isArray(reply.status))
      throw new Error("History status reply is invalid.");
    const status = reply.status as Record<string, unknown>;
    const prune = status.last_prune;
    if (!prune || typeof prune !== "object" || Array.isArray(prune))
      throw new Error("History retention status is invalid.");
    const retention = prune as Record<string, unknown>;
    for (const key of ["available", "configuration_ready", "enabled", "effective_enabled", "degraded"])
      if (typeof status[key] !== "boolean") throw new Error(`History ${key} status is invalid.`);
    for (const key of ["failed_operations", "active_streams", "sessions", "samples", "events", "live_bytes"])
      if (!Number.isSafeInteger(status[key]) || (status[key] as number) < 0)
        throw new Error(`History ${key} count is invalid.`);
    for (const key of ["sessions_deleted", "live_bytes", "size_limit_bytes"])
      if (!Number.isSafeInteger(retention[key]) || (retention[key] as number) < 0)
        throw new Error(`History retention ${key} count is invalid.`);
    if (typeof retention.size_limit_unmet !== "boolean")
      throw new Error("History retention limit status is invalid.");
    return status as unknown as HistoryRuntimeStatus;
  }

  export function historyRuntimeDescription(status: HistoryRuntimeStatus): string[] {
    if (!status.available)
      return ["History storage is unavailable; recording and recorded session reads cannot run."];
    const lines = [status.configuration_ready
      ? status.effective_enabled === status.enabled
        ? status.effective_enabled
          ? "Recording is enabled for new streams."
          : "Recording is disabled for new streams."
        : status.enabled
          ? "Recording enable is deferred until active streams end; new streams still use the disabled policy."
          : "Recording disable is deferred until active streams end; new streams still use the enabled policy."
      : "History configuration is not ready; recording is not confirmed."];
    lines.push(`${status.active_streams} active streams · ${status.sessions} stored sessions · ${status.samples} samples · ${status.events} events · ${status.live_bytes} live bytes.`);
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
    for (const key of ["sample_count", "event_count"])
      if (!Number.isSafeInteger(detail[key]) || (detail[key] as number) < 0)
        throw new Error(`History ${key} is invalid.`);
    for (const key of ["samples_truncated", "events_truncated"])
      if (typeof detail[key] !== "boolean") throw new Error(`History ${key} is invalid.`);
    for (const [key, countKey] of [["samples", "sample_count"], ["events", "event_count"]] as const) {
      const rows = detail[key];
      if (!Array.isArray(rows) || rows.length > (detail[countKey] as number) || rows.some((row) =>
        !row || typeof row !== "object" || Array.isArray(row) || row.session_id !== sessionId ||
        !Number.isSafeInteger(row.recorded_at_unix)))
        throw new Error(`History ${key} are invalid.`);
    }
    for (const sample of detail.samples as Record<string, unknown>[]) {
      for (const key of ["frames_sent", "bytes_sent", "dropped_frames", "actual_fps", "actual_bitrate_kbps"])
        if (typeof sample[key] !== "number" || !Number.isFinite(sample[key]) || sample[key] < 0)
          throw new Error(`History sample ${key} is invalid.`);
    }
    if ((detail.events as Record<string, unknown>[]).some((event) => typeof event.event_type !== "string"))
      throw new Error("History event type is invalid.");
    return reply as unknown as HistoryDetailReply;
  }

  export function renderSessions(context: PageContext): void {
    const area = el("div");
    const status = el("div");
    const historyRuntimeArea = el("div");
    const savedArea = el("div");
    const savedDetail = el("div");
    const savedStatus = el("div");
    let offset = 0;
    const history = [0];
    let savedOffset = 0;
    const savedHistory = [0];
    let savedListRequest = 0;
    let savedDetailRequest = 0;
    let historyStatusRequest = 0;
    let historyDeleteAvailable = false;
    let historyDeleteReason = "Checking history status before deletion is available.";
    let deleteControls: { button: HTMLButtonElement; ended: boolean }[] = [];
    const deleteNotice = el("p", "muted", historyDeleteReason);
    let selectedSavedSession: string | null = null;
    context.content.append(
      sectionHeading(
        "Sessions",
        "Inspect and stop sessions owned by this management console.",
        button("Refresh", () => {
          void load();
          void loadHistoryRuntime();
          void loadSaved();
        }),
      ),
      status,
      area,
      panel("History runtime status", historyRuntimeArea),
      panel("Recorded stream sessions", savedArea, savedStatus, savedDetail),
    );
    function applyHistoryDeleteAvailability(): void {
      deleteNotice.textContent = historyDeleteReason;
      for (const control of deleteControls)
        control.button.disabled = !control.ended || !context.api.allows("session.write") || !historyDeleteAvailable;
    }
    async function loadHistoryRuntime(): Promise<boolean> {
      const request = ++historyStatusRequest;
      historyDeleteAvailable = false;
      historyDeleteReason = "Checking history status before deletion is available.";
      applyHistoryDeleteAvailability();
      historyRuntimeArea.replaceChildren(el("p", "muted", "Loading history runtime status…"));
      try {
        const runtime = parseHistoryRuntimeStatus(await context.api.post<unknown>("history/history_status"));
        if (!context.content.isConnected || request !== historyStatusRequest) return false;
        historyDeleteAvailable = runtime.available && runtime.configuration_ready && runtime.effective_enabled;
        historyDeleteReason = historyDeleteAvailable
          ? ""
          : runtime.available && runtime.configuration_ready && !runtime.effective_enabled
            ? "Deletion is unavailable while history recording is disabled. Stored sessions remain readable."
            : "Deletion is unavailable until history storage and configuration are ready.";
        applyHistoryDeleteAvailability();
        historyRuntimeArea.replaceChildren(
          ...historyRuntimeDescription(runtime).map((line) => el("p", "muted", line)),
          button("Refresh history status", () => { void loadHistoryRuntime(); }),
        );
        return historyDeleteAvailable;
      } catch (error) {
        if (context.content.isConnected && request === historyStatusRequest) {
          historyDeleteReason = "Deletion is unavailable until history runtime status can be confirmed. Stored sessions remain readable.";
          applyHistoryDeleteAvailability();
          historyRuntimeArea.replaceChildren(errorPanel(error, () => { void loadHistoryRuntime(); }));
        }
        return false;
      }
    }
    async function load(): Promise<void> {
      try {
        const reply = await context.api.post<SessionReply>("session/list", {
          offset,
          limit: 16,
        });
        if (!context.content.isConnected) return;
        const rows = reply.sessions.map((session) => {
          const stop = button(
            session.state === "stopping" ? "Finish stop" : "Stop session",
            async () => {
              if (
                !(await confirmAction(
                  "Stop this session?",
                  `Session “${session.key}” will be moved toward its stopped state.`,
                  session.state === "stopping" ? "Finish stop" : "Stop session",
                ))
              )
                return;
              await runAction(stop, status, async () => {
                await context.api.post(
                  `session/${session.state === "stopping" ? "finish_stop" : "stop"}`,
                  { session_id: session.id },
                );
                context.notify("Session updated.");
                await load();
              });
            },
          );
          stop.disabled =
            session.state === "stopped" || !context.api.allows("session.write");
          return [
            el(
              "div",
              "",
              el("strong", "", session.key),
              el("small", "muted block", `Session ${session.id}`),
            ),
            badge(session.state, session.state === "active" ? "good" : ""),
            String(
              session.lease_count ?? Object.keys(session.leases ?? {}).length,
            ),
            stop,
          ];
        });
        area.replaceChildren(
          reply.sessions.length
            ? table(["Session", "State", "Leases", "Action"], rows)
            : empty(
                "No sessions yet",
                "Sessions created through the management API appear here. Streaming clients and pairing are not available yet.",
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
            `Showing from ${offset + 1}`,
          ),
        );
      } catch (error) {
        if (context.content.isConnected)
          area.replaceChildren(errorPanel(error, load));
      }
    }
    async function loadSaved(): Promise<void> {
      const request = ++savedListRequest;
      savedArea.replaceChildren(el("p", "muted", "Loading recorded sessions…"));
      try {
        const reply = await context.api.post<HistoryListReply>(
          "history/list_sessions",
          { limit: 25, offset: savedOffset },
        );
        if (!context.content.isConnected || request !== savedListRequest) return;
        if (reply.ok !== true || !Array.isArray(reply.sessions) ||
          (reply.next_offset != null && (!Number.isSafeInteger(reply.next_offset) || reply.next_offset <= savedOffset)))
          throw new Error("History session page is invalid.");
        const currentDeleteControls: typeof deleteControls = [];
        const rows = reply.sessions.map((session) => {
          const details = button("Details", () => loadSavedDetail(session.session_id));
          const remove = button(
            "Delete",
            async () => {
              if (!historyDeleteAvailable || !(await loadHistoryRuntime())) return;
              if (
                !(await confirmAction(
                  "Delete this recorded session?",
                  `The history for ${session.app_name || session.session_id} will be removed.`,
                  "Delete history",
                ))
              )
                return;
              // The writer may have drained while the confirmation was open.
              if (!historyDeleteAvailable || !(await loadHistoryRuntime())) return;
              await runAction(remove, savedStatus, async () => {
                await context.api.post("history/delete_session", {
                  session_id: session.session_id,
                });
                if (selectedSavedSession === session.session_id) {
                  ++savedDetailRequest;
                  selectedSavedSession = null;
                  savedDetail.replaceChildren();
                }
                context.notify("Recorded session deleted.");
                await loadSaved();
              });
              await loadHistoryRuntime();
            },
            "quiet danger-text",
          );
          currentDeleteControls.push({ button: remove, ended: session.ended_at_unix != null });
          return [
            el("div", "", el("strong", "", session.app_name || session.session_id),
              el("small", "muted block", session.client_name || session.protocol)),
            new Date(session.started_at_unix * 1000).toLocaleString(),
            session.ended_at_unix == null ? badge("Active", "good") :
              `${session.duration_seconds ?? 0} seconds`,
            el("div", "actions", details, remove),
          ];
        });
        deleteControls = currentDeleteControls;
        applyHistoryDeleteAvailability();
        savedArea.replaceChildren(
          deleteNotice,
          reply.sessions.length
            ? table(["Application / client", "Started", "Duration", "Actions"], rows)
            : empty("No recorded streams", "Completed streams appear here when history recording is enabled."),
          pager(
            savedHistory.length > 1
              ? () => {
                  savedHistory.pop();
                  savedOffset = savedHistory[savedHistory.length - 1] ?? 0;
                  void loadSaved();
                }
              : null,
            reply.next_offset != null
              ? () => {
                  savedOffset = reply.next_offset!;
                  savedHistory.push(savedOffset);
                  void loadSaved();
                }
              : null,
            `Showing from ${savedOffset + 1}`,
          ),
        );
      } catch (error) {
        if (context.content.isConnected && request === savedListRequest)
          savedArea.replaceChildren(errorPanel(error, loadSaved));
      }
    }

    async function loadSavedDetail(sessionId: string): Promise<void> {
      selectedSavedSession = sessionId;
      const request = ++savedDetailRequest;
      savedDetail.replaceChildren(el("p", "muted", "Loading session details…"));
      try {
        const reply = parseHistoryDetailReply(await context.api.post<unknown>(
          "history/session_detail",
          { session_id: sessionId },
        ), sessionId);
        if (!context.content.isConnected || request !== savedDetailRequest) return;
        const detail = reply.detail;
        if (!detail) return;
        savedDetail.replaceChildren(
          panel(
            `History for ${detail.session.app_name || detail.session.session_id}`,
            el("p", "muted", `${detail.session.protocol} · ${detail.session.width}×${detail.session.height} · ${detail.session.target_fps} FPS · ${detail.session.codec || "codec unavailable"}`),
            el("p", "muted", `${detail.sample_count} samples${detail.samples_truncated ? " (recent samples shown)" : ""} · ${detail.event_count} events${detail.events_truncated ? " (recent events shown)" : ""}`),
            detail.samples.length
              ? table(
                  ["Sample time", "Frames", "Bytes", "Dropped", "FPS", "Bitrate (kbps)"],
                  detail.samples.map((sample) => [
                    new Date(sample.recorded_at_unix * 1000).toLocaleString(),
                    String(sample.frames_sent),
                    String(sample.bytes_sent),
                    String(sample.dropped_frames),
                    sample.actual_fps.toFixed(1),
                    sample.actual_bitrate_kbps.toFixed(1),
                  ]),
                )
              : el("p", "muted", "No samples recorded."),
            detail.events.length
              ? table(
                  ["Event time", "Event"],
                  detail.events.map((event) => [
                    new Date(event.recorded_at_unix * 1000).toLocaleString(),
                    event.event_type,
                  ]),
                )
              : el("p", "muted", "No events recorded."),
          ),
        );
      } catch (error) {
        if (context.content.isConnected && request === savedDetailRequest)
          savedDetail.replaceChildren(errorPanel(error, () => loadSavedDetail(sessionId)));
      }
    }
    void load();
    void loadHistoryRuntime();
    void loadSaved();
  }

  export function renderApplications(context: PageContext): void {
    const area = el("div");
    const status = el("div");
    let offset = 0;
    const history = [0];
    context.content.append(
      sectionHeading(
        "Applications",
        "Processes launched through this management console.",
        button("Refresh", load),
      ),
      status,
      area,
    );
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
                  `Stop process ${process.pid ?? process.process_id} and its owned process tree. Unsaved work may be lost.`,
                  "Terminate",
                ))
              )
                return;
              await runAction(terminate, status, async () => {
                await context.api.post("application/terminate", {
                  process_id: process.process_id,
                });
                context.notify(
                  "Termination requested. Refresh to check its status.",
                );
                await load();
              });
            },
            "quiet danger-text",
          );
          terminate.disabled =
            process.state !== "running" ||
            !context.api.allows("application.terminate");
          return [
            el("strong", "", process.operation_id),
            String(process.pid ?? "—"),
            badge(process.state, process.state === "running" ? "good" : ""),
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
            `Showing from ${offset + 1}`,
          ),
        );
      } catch (error) {
        if (context.content.isConnected)
          area.replaceChildren(errorPanel(error, load));
      }
    }
    void load();
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
          context.notify(
            "Steam scan requested. Refresh to see completed discoveries.",
          );
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
      status.replaceChildren(el("p", "notice warning", message));
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
        context.notify("Steam discovery completed and catalog sync succeeded.");
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
      status.replaceChildren(el("p", "notice warning", message));
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
            el("strong", "", game.name),
            el("small", "path block", game.install_directory ?? ""),
          ),
          String(game.app_id),
          badge(game.installed ? "Installed" : "Discovered"),
        ]);
        area.replaceChildren(
          el(
            "div",
            "section-meta",
            badge(
              provider.enabled === false
                ? "Discovery disabled"
                : provider.scanning
                ? "Scan in progress"
                : provider.complete
                  ? "Scan complete"
                  : "Scan incomplete",
            ),
            el(
              "span",
              "muted",
              `${provider.game_count} discoveries · revision ${reply.revision}`,
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
            el("p", "notice", warning),
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
            `Page ${history.length}`,
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

  export function renderDiagnostics(context: PageContext): void {
    const area = el("div");
    let after = 0;
    const history = [0];
    context.content.append(
      sectionHeading(
        "Diagnostics",
        "Records and counters for this management console.",
        button("Refresh", () => {
          after = 0;
          history.splice(0, history.length, 0);
          return load();
        }),
      ),
      area,
    );
    async function load(): Promise<void> {
      try {
        const [reply, counters] = await Promise.all([
          context.api.post<DiagnosticReply>("diagnostics/query", {
            after,
            limit: 4,
          }),
          context.api.post<{ counters: Record<string, number> }>(
            "diagnostics/counters",
          ),
        ]);
        if (!context.content.isConnected) return;
        const records = el(
          "div",
          "records",
          ...reply.records.map((record) =>
            el(
              "article",
              "record",
              el(
                "div",
                "section-meta",
                badge(record.level, record.level === "error" ? "bad" : ""),
                el("small", "muted", `#${record.sequence} · ${record.target}`),
              ),
              el("p", "", record.message),
              ...Object.entries(record.fields).map(([key, value]) =>
                el("p", "record-field", el("strong", "", `${key}: `), value),
              ),
            ),
          ),
        );
        area.replaceChildren(
          reply.resync_required
            ? el(
                "p",
                "notice",
                "Older records were removed from the bounded journal. Showing the records still available.",
              )
            : nullNode(),
          panel(
            "Recent records",
            reply.records.length
              ? records
              : empty(
                  "No diagnostic records",
                  "Records appear when a service explicitly submits them. Automatic host log collection is not enabled.",
                ),
          ),
          pager(
            history.length > 1
              ? () => {
                  history.pop();
                  after = history[history.length - 1] ?? 0;
                  void load();
                }
              : null,
            reply.has_more
              ? () => {
                  after = reply.next_cursor;
                  history.push(after);
                  void load();
                }
              : null,
            `Page ${history.length}`,
          ),
          panel(
            "Counters",
            Object.keys(counters.counters).length
              ? table(
                  ["Counter", "Value"],
                  Object.entries(counters.counters).map(([name, value]) => [
                    name,
                    String(value),
                  ]),
                )
              : el("p", "muted", "No counters have been recorded."),
          ),
        );
      } catch (error) {
        if (context.content.isConnected)
          area.replaceChildren(errorPanel(error, load));
      }
    }
    void load();
  }
  function nullNode(): Text {
    return document.createTextNode("");
  }
}
