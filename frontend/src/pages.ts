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
    let offset = 0;
    const history = [0];
    context.content.append(
      sectionHeading(
        "Sessions",
        "Inspect and stop sessions owned by this management console.",
        button("Refresh", load),
      ),
      status,
      area,
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
    void load();
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

  export function renderSteam(context: PageContext): void {
    const area = el("div");
    const status = el("div");
    let revision: number | undefined;
    let after: number | undefined;
    const history: (number | undefined)[] = [undefined];
    const scan = button(
      "Scan libraries",
      () =>
        runAction(scan, status, async () => {
          await context.api.post("steam/refresh");
          revision = undefined;
          after = undefined;
          history.splice(0, history.length, undefined);
          context.notify(
            "Steam scan requested. Refresh to see completed discoveries.",
          );
          await load();
        }),
      "primary",
    );
    scan.disabled = !context.api.allows("steam.refresh");
    context.content.append(
      sectionHeading(
        "Steam library",
        "Read-only discovery from the Steam roots configured on this host.",
        button("Refresh", () => {
          revision = undefined;
          after = undefined;
          history.splice(0, history.length, undefined);
          return load();
        }),
        scan,
      ),
      status,
      area,
    );
    async function load(): Promise<void> {
      try {
        const [reply, provider] = await Promise.all([
          context.api.post<SteamReply>("steam/list", {
            ...(revision != null ? { revision } : {}),
            ...(after != null ? { after } : {}),
            limit: 16,
          }),
          context.api.post<SteamStatus>("steam/status"),
        ]);
        if (!context.content.isConnected) return;
        revision = reply.revision;
        scan.disabled =
          !context.api.allows("steam.refresh") || provider.enabled === false;
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
