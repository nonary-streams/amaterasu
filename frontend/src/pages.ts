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

  export function renderSessions(context: PageContext): void {
    const area = el("div");
    const status = el("div");
    const savedArea = el("div");
    const savedDetail = el("div");
    const savedStatus = el("div");
    let offset = 0;
    const history = [0];
    let savedOffset = 0;
    context.content.append(
      sectionHeading(
        "Sessions",
        "Inspect and stop sessions owned by this management console.",
        button("Refresh", () => {
          void load();
          void loadSaved();
        }),
      ),
      status,
      area,
      panel("Recorded stream sessions", savedArea, savedStatus, savedDetail),
    );
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
      savedArea.replaceChildren(el("p", "muted", "Loading recorded sessions…"));
      try {
        const reply = await context.api.post<HistoryListReply>(
          "history/list_sessions",
          { limit: 25, offset: savedOffset },
        );
        if (!context.content.isConnected) return;
        const rows = reply.sessions.map((session) => {
          const details = button("Details", () => loadSavedDetail(session.session_id));
          const remove = button(
            "Delete",
            async () => {
              if (
                !(await confirmAction(
                  "Delete this recorded session?",
                  `The history for ${session.app_name || session.session_id} will be removed.`,
                  "Delete history",
                ))
              )
                return;
              await runAction(remove, savedStatus, async () => {
                await context.api.post("history/delete_session", {
                  session_id: session.session_id,
                });
                savedDetail.replaceChildren();
                context.notify("Recorded session deleted.");
                await loadSaved();
              });
            },
            "quiet danger-text",
          );
          remove.disabled =
            session.ended_at_unix == null || !context.api.allows("session.write");
          return [
            el("div", "", el("strong", "", session.app_name || session.session_id),
              el("small", "muted block", session.client_name || session.protocol)),
            new Date(session.started_at_unix * 1000).toLocaleString(),
            session.ended_at_unix == null ? badge("Active", "good") :
              `${session.duration_seconds ?? 0} seconds`,
            el("div", "actions", details, remove),
          ];
        });
        savedArea.replaceChildren(
          reply.sessions.length
            ? table(["Application / client", "Started", "Duration", "Actions"], rows)
            : empty("No recorded streams", "Completed streams appear here when history recording is enabled."),
          pager(
            savedOffset > 0
              ? () => {
                  savedOffset = Math.max(0, savedOffset - 25);
                  void loadSaved();
                }
              : null,
            reply.sessions.length === 25
              ? () => {
                  savedOffset += 25;
                  void loadSaved();
                }
              : null,
            `Showing from ${savedOffset + 1}`,
          ),
        );
      } catch (error) {
        if (context.content.isConnected)
          savedArea.replaceChildren(errorPanel(error, loadSaved));
      }
    }

    async function loadSavedDetail(sessionId: string): Promise<void> {
      savedDetail.replaceChildren(el("p", "muted", "Loading session details…"));
      try {
        const reply = await context.api.post<HistoryDetailReply>(
          "history/session_detail",
          { session_id: sessionId },
        );
        if (!context.content.isConnected) return;
        const detail = reply.detail;
        if (!detail) {
          savedDetail.replaceChildren(empty("Session unavailable", "The recorded session was removed."));
          return;
        }
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
        if (context.content.isConnected)
          savedDetail.replaceChildren(errorPanel(error, () => loadSavedDetail(sessionId)));
      }
    }
    void load();
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

  type SteamForceSyncReply =
    | (SteamForceSyncStatus & { ok: true; accepted: true })
    | { ok: false; code: string };

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
        if (reply.ok !== true || reply.accepted !== true) {
          throw new Error(
            reply.code
              ? `Force catalog sync was not accepted: ${reply.code}`
              : "Force catalog sync was not accepted.",
          );
        }
        if (
          reply.force_sync_pending !== true ||
          reply.catalog_sync.force_sync_pending !== true
        ) {
          throw new Error("Force catalog sync returned no pending status.");
        }

        forceSyncPending = true;
        providerStatus = reply;
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
