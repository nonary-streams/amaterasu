namespace Amaterasu {
  function hostMetricsTextLiteral(value: string): TextValue {
    return typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(value) : value;
  }

  function hostMetricsTextMessage(value: string): TextValue {
    return typeof Amaterasu.localizedMessage === "function" ? Amaterasu.localizedMessage(value) : value;
  }

  function hostMetricsSetLocalizedTextContent(
    target: HTMLElement,
    source: string,
  ): void {
    if (typeof Amaterasu.setLocalizedTextContent === "function") {
      Amaterasu.setLocalizedTextContent(target, source);
    } else {
      target.textContent = source;
    }
  }

  interface RealtimeSettings {
    enabled: boolean;
    poll_interval_ms: number;
    history_retention_seconds: number;
    max_history_points: number;
    pause_when_hidden: boolean;
    show_active_sessions: boolean;
    show_host_stats: boolean;
    show_host_charts: boolean;
    show_session_history: boolean;
  }

  interface RealtimeSettingsReply {
    ok: true;
    settings: RealtimeSettings;
    revision: number;
  }

  interface HostSnapshot {
    cpu_percent: number | null;
    cpu_temp_c: number | null;
    ram_used_bytes: number | null;
    ram_total_bytes: number | null;
    gpu_percent: number | null;
    gpu_encoder_percent: number | null;
    gpu_temp_c: number | null;
    vram_used_bytes: number | null;
    vram_total_bytes: number | null;
    net_rx_bps: number | null;
    net_tx_bps: number | null;
  }

  interface HostSample {
    recorded_at_unix_ms: number;
    host: HostSnapshot;
  }

  interface HostStatsReply {
    ok: true;
    available: boolean;
    platform: string;
    sampled_at_unix_ms: number | null;
    host: HostSnapshot;
    samples: HostSample[];
  }

  const realtimeDefaults: RealtimeSettings = {
    enabled: true,
    poll_interval_ms: 2_000,
    history_retention_seconds: 300,
    max_history_points: 300,
    pause_when_hidden: true,
    show_active_sessions: true,
    show_host_stats: true,
    show_host_charts: true,
    show_session_history: true,
  };
  const dashboardCleanup = new WeakMap<HTMLElement, () => void>();
  const settingsGeneration = new WeakMap<HTMLElement, number>();

  function parseRealtimeSettings(value: unknown): RealtimeSettings {
    if (value === null || typeof value !== "object" || Array.isArray(value))
      throw new Error("Real-time statistics settings are invalid.");
    const settings = value as Record<string, unknown>;
    const knownKeys = new Set([
      "enabled", "poll_interval_ms", "history_retention_seconds", "max_history_points",
      "pause_when_hidden", "show_active_sessions", "show_host_stats", "show_host_charts",
      "show_session_history",
    ]);
    if (Object.keys(settings).length !== knownKeys.size || Object.keys(settings).some((key) => !knownKeys.has(key)))
      throw new Error("Real-time statistics settings contain unknown or missing fields.");
    const booleans = [
      "enabled", "pause_when_hidden", "show_active_sessions", "show_host_stats",
      "show_host_charts", "show_session_history",
    ] as const;
    if (booleans.some((key) => typeof settings[key] !== "boolean"))
      throw new Error("Real-time statistics display settings are invalid.");
    const boundedInteger = (key: string, min: number, max: number): number => {
      const value = settings[key];
      if (typeof value !== "number" || !Number.isSafeInteger(value) || value < min || value > max)
        throw new Error(`Real-time statistics ${key} must be a whole number from ${min} to ${max}.`);
      return value;
    };
    return {
      enabled: settings.enabled as boolean,
      poll_interval_ms: boundedInteger("poll_interval_ms", 250, 60_000),
      history_retention_seconds: boundedInteger("history_retention_seconds", 30, 3_600),
      max_history_points: boundedInteger("max_history_points", 30, 2_000),
      pause_when_hidden: settings.pause_when_hidden as boolean,
      show_active_sessions: settings.show_active_sessions as boolean,
      show_host_stats: settings.show_host_stats as boolean,
      show_host_charts: settings.show_host_charts as boolean,
      show_session_history: settings.show_session_history as boolean,
    };
  }

  function parseRealtimeSettingsReply(value: unknown): RealtimeSettingsReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Real-time statistics settings reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || typeof reply.revision !== "number" ||
        !Number.isSafeInteger(reply.revision) || reply.revision < 0)
      throw new Error("Real-time statistics settings reply is invalid.");
    return {
      ok: true,
      revision: reply.revision,
      settings: parseRealtimeSettings(reply.settings),
    };
  }

  function parseHostSnapshot(value: unknown): HostSnapshot {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Host metrics snapshot is invalid.");
    const source = value as Record<string, unknown>;
    const result = {} as HostSnapshot;
    for (const key of [
      "cpu_percent", "cpu_temp_c", "ram_used_bytes", "ram_total_bytes", "gpu_percent",
      "gpu_encoder_percent", "gpu_temp_c", "vram_used_bytes", "vram_total_bytes",
      "net_rx_bps", "net_tx_bps",
    ] as const) {
      const metric = source[key];
      if (metric !== null && (typeof metric !== "number" || !Number.isFinite(metric) || metric < 0))
        throw new Error(`Host metric ${key} is invalid.`);
      result[key] = metric as number | null;
    }
    return result;
  }

  function parseHostStatsReply(value: unknown): HostStatsReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Host statistics reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || typeof reply.available !== "boolean" || typeof reply.platform !== "string" ||
        (reply.sampled_at_unix_ms !== null && (!Number.isSafeInteger(reply.sampled_at_unix_ms) ||
          (reply.sampled_at_unix_ms as number) < 0)) || !Array.isArray(reply.samples) || reply.samples.length > 32)
      throw new Error("Host statistics reply is invalid.");
    const samples = reply.samples.map((sample): HostSample => {
      if (!sample || typeof sample !== "object" || Array.isArray(sample))
        throw new Error("Host statistics history point is invalid.");
      const point = sample as Record<string, unknown>;
      if (!Number.isSafeInteger(point.recorded_at_unix_ms) || (point.recorded_at_unix_ms as number) < 0)
        throw new Error("Host statistics history timestamp is invalid.");
      return { recorded_at_unix_ms: point.recorded_at_unix_ms as number, host: parseHostSnapshot(point.host) };
    });
    return {
      ok: true,
      available: reply.available,
      platform: reply.platform,
      sampled_at_unix_ms: reply.sampled_at_unix_ms as number | null,
      host: parseHostSnapshot(reply.host),
      samples,
    };
  }

  async function loadRealtimeSettings(api: Pick<Api, "post">): Promise<RealtimeSettingsReply> {
    return parseRealtimeSettingsReply(await api.post<unknown>("host-metrics/settings"));
  }

  async function saveRealtimeSettings(
    api: Pick<Api, "post">,
    current: RealtimeSettingsReply,
    settings: RealtimeSettings,
  ): Promise<RealtimeSettingsReply> {
    const desired = parseRealtimeSettings(settings);
    const updated = parseRealtimeSettingsReply(await api.post<unknown>("host-metrics/settings_update", {
      settings: desired,
      expected_revision: current.revision,
    }));
    if (updated.revision !== current.revision + 1 ||
        JSON.stringify(updated.settings) !== JSON.stringify(desired))
      throw new Error("Real-time statistics settings update could not be confirmed. Refresh and check the saved values.");
    const saved = await loadRealtimeSettings(api);
    if (saved.revision !== updated.revision || JSON.stringify(saved.settings) !== JSON.stringify(desired))
      throw new Error("Real-time statistics settings could not be confirmed after saving.");
    return saved;
  }

  export function renderHostMetricsSettings(context: PageContext, container: HTMLElement): void {
    const generation = (settingsGeneration.get(container) ?? 0) + 1;
    settingsGeneration.set(container, generation);
    let current: RealtimeSettingsReply | null = null;
    async function load(): Promise<void> {
      container.replaceChildren(el("p", "muted", "Loading real-time statistics settings…"));
      try {
        const loaded = await loadRealtimeSettings(context.api);
        if (!container.isConnected || settingsGeneration.get(container) !== generation) return;
        current = loaded;
        const settings = current.settings;
        const form = el("form", "settings-form");
        const enabled = input("realtime_stats_enabled", "", "checkbox");
        enabled.checked = settings.enabled;
        const poll = input("realtime_stats_poll_interval_ms", String(settings.poll_interval_ms), "number");
        const retention = input("realtime_stats_history_retention_seconds", String(settings.history_retention_seconds), "number");
        const points = input("realtime_stats_max_history_points", String(settings.max_history_points), "number");
        for (const control of [poll, retention, points]) {
          control.required = true;
          control.step = "1";
        }
        poll.min = "250";
        poll.max = "60000";
        retention.min = "30";
        retention.max = "3600";
        points.min = "30";
        points.max = "2000";
        const pause = input("realtime_stats_pause_when_hidden", "", "checkbox");
        pause.checked = settings.pause_when_hidden;
        const active = input("realtime_stats_show_active_sessions", "", "checkbox");
        active.checked = settings.show_active_sessions;
        const hostStats = input("realtime_stats_show_host_stats", "", "checkbox");
        hostStats.checked = settings.show_host_stats;
        const charts = input("realtime_stats_show_host_charts", "", "checkbox");
        charts.checked = settings.show_host_charts;
        const sessionHistory = input("realtime_stats_show_session_history", "", "checkbox");
        sessionHistory.checked = settings.show_session_history;
        const status = el("p", "muted", hostMetricsTextLiteral(`Saved settings revision ${current.revision}.`));
        const save = el("button", "button primary", "Save real-time statistics settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          field("Enable real-time statistics", enabled,
            "Collect measured host metrics for the live statistics dashboard and while an active stream requests samples."),
          field("Sampling and dashboard interval (ms)", poll, "250 to 60000 milliseconds."),
          field("Chart history retention (seconds)", retention, "30 to 3600 seconds; history is held in memory."),
          field("Maximum retained samples", points,
            "Retain 30 to 2000 measured samples in memory; the live response spreads at most 32 chart points across that history."),
          field("Pause after the dashboard is hidden", pause,
            "When enabled, sampling pauses after the visible dashboard lease expires unless a stream is active."),
          field("Show active sessions", active),
          field("Show host statistics", hostStats),
          field("Show host charts", charts),
          field("Show session history", sessionHistory),
          status,
          el("div", "actions", save, badge(hostMetricsTextLiteral(`Revision ${current.revision}`))),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            if (!current) throw new Error("Statistics settings must be refreshed before saving.");
            current = await saveRealtimeSettings(context.api, current, {
              enabled: enabled.checked,
              poll_interval_ms: poll.valueAsNumber,
              history_retention_seconds: retention.valueAsNumber,
              max_history_points: points.valueAsNumber,
              pause_when_hidden: pause.checked,
              show_active_sessions: active.checked,
              show_host_stats: hostStats.checked,
              show_host_charts: charts.checked,
              show_session_history: sessionHistory.checked,
            });
            context.notify(`Real-time statistics settings revision ${current.revision} saved.`);
            await load();
          });
        });
        container.replaceChildren(panel("Real-time statistics", form));
      } catch (error) {
        if (container.isConnected && settingsGeneration.get(container) === generation)
          container.replaceChildren(errorPanel(error, load));
      }
    }
    void load();
  }

  export function renderHostMetricsDashboard(
    context: PageContext,
    container: HTMLElement,
    sessionHistoryContainer?: HTMLElement,
  ): void {
    dashboardCleanup.get(container)?.();
    let requestGeneration = 0;
    let timer: number | undefined;
    let disposed = false;
    let observer: MutationObserver | undefined;
    let settings: RealtimeSettings = realtimeDefaults;
    const area = el("div");
    const status = el("p", "muted", "Loading live host metrics…");
    container.replaceChildren(button("Refresh host metrics", () => void refresh()), area, status);

    const cleanup = (): void => {
      if (disposed) return;
      disposed = true;
      ++requestGeneration;
      if (timer !== undefined) window.clearTimeout(timer);
      timer = undefined;
      document.removeEventListener("visibilitychange", applyVisibility);
      observer?.disconnect();
      dashboardCleanup.delete(container);
    };
    dashboardCleanup.set(container, cleanup);

    function applyVisibility(): void {
      if (disposed || !container.isConnected) return;
      if (sessionHistoryContainer)
        sessionHistoryContainer.hidden = !settings.show_session_history;
      if (!settings.enabled) {
        renderDisabledState();
        return;
      }
      if (settings.pause_when_hidden && document.visibilityState !== "visible") {
        if (timer !== undefined) window.clearTimeout(timer);
        timer = undefined;
      } else if (settings.enabled && timer === undefined) {
        timer = window.setTimeout(() => void refresh(), 0);
      }
    }

    async function refresh(): Promise<void> {
      timer = undefined;
      const request = ++requestGeneration;
      if (disposed || !container.isConnected) return;
      if (!settings.enabled) {
        renderDisabledState();
        return;
      }
      if (settings.pause_when_hidden && document.visibilityState !== "visible") return;
      try {
        const result = parseHostStatsReply(await context.api.post<unknown>("host-metrics/stats"));
        if (disposed || !container.isConnected || request !== requestGeneration ||
            settings.pause_when_hidden && document.visibilityState !== "visible") return;
        const panels: HTMLElement[] = [];
        if (settings.show_host_stats)
          panels.push(renderHostStats(result));
        if (settings.show_host_charts)
          panels.push(renderHostCharts(result.samples));
        if (settings.show_active_sessions)
          panels.push(await renderActiveSessions(context));
        if (disposed || !container.isConnected || request !== requestGeneration ||
            settings.pause_when_hidden && document.visibilityState !== "visible") return;
        area.replaceChildren(...panels);
        const locale =
          typeof Amaterasu.activeIntlLocale === "function"
            ? Amaterasu.activeIntlLocale()
            : undefined;
        status.textContent = result.available
          ? result.sampled_at_unix_ms === null
            ? `Host telemetry is available on ${result.platform}; waiting for its first measured sample.`
            : `Measured ${result.platform} host values at ${new Date(result.sampled_at_unix_ms).toLocaleTimeString(locale)}. Missing sensors are shown as unavailable.`
          : `Host telemetry is unsupported on this platform (${result.platform}); no sample values were invented.`;
      } catch (error) {
        if (!disposed && container.isConnected && request === requestGeneration)
          area.replaceChildren(errorPanel(error));
      }
      schedule();
    }

    function schedule(): void {
      if (disposed || !container.isConnected || !settings.enabled || timer !== undefined) return;
      if (settings.pause_when_hidden && document.visibilityState !== "visible") return;
      timer = window.setTimeout(() => void refresh(), settings.poll_interval_ms);
    }

    function renderDisabledState(): void {
      area.replaceChildren(empty(
        "Real-time statistics are disabled",
        "Enable collection in Settings to show live host telemetry.",
      ));
      hostMetricsSetLocalizedTextContent(status, "Sampling is disabled.");
    }

    async function initialize(): Promise<void> {
      try {
        const loaded = await loadRealtimeSettings(context.api);
        if (disposed || !container.isConnected) return;
        settings = loaded.settings;
        if (sessionHistoryContainer)
          sessionHistoryContainer.hidden = !settings.show_session_history;
        if (!settings.enabled) {
          renderDisabledState();
          return;
        }
        applyVisibility();
      } catch (error) {
        if (!disposed && container.isConnected) area.replaceChildren(errorPanel(error));
      }
    }

    document.addEventListener("visibilitychange", applyVisibility);
    observer = new MutationObserver(() => {
      if (!container.isConnected) cleanup();
    });
    observer.observe(document.body, { childList: true, subtree: true });
    void initialize();
  }

  function renderHostStats(result: HostStatsReply): HTMLElement {
    const host = result.host;
    const percent = (value: number | null): string => value === null ? "Unavailable" : `${value.toFixed(1)}%`;
    const temperature = (value: number | null): string => value === null ? "Unavailable" : `${value.toFixed(1)} °C`;
    const bytes = (used: number | null, total: number | null): string =>
      used === null || total === null ? "Unavailable" : `${formatBytes(used)} / ${formatBytes(total)}`;
    const rate = (value: number | null): string => value === null ? "Unavailable" : `${(value / 1_000_000).toFixed(2)} Mbit/s`;
    return panel("Current host metrics", table(["Metric", "Measured value"], [
      ["CPU utilization", percent(host.cpu_percent)],
      ["CPU temperature", temperature(host.cpu_temp_c)],
      ["Memory used / total", bytes(host.ram_used_bytes, host.ram_total_bytes)],
      ["GPU utilization", percent(host.gpu_percent)],
      ["GPU encoder utilization", percent(host.gpu_encoder_percent)],
      ["GPU temperature", temperature(host.gpu_temp_c)],
      ["Video memory used / total", bytes(host.vram_used_bytes, host.vram_total_bytes)],
      ["Network receive", rate(host.net_rx_bps)],
      ["Network transmit", rate(host.net_tx_bps)],
    ]));
  }

  function renderHostCharts(samples: HostSample[]): HTMLElement {
    const charts = [
      { label: "CPU utilization", values: samples.map((sample) => sample.host.cpu_percent), maximum: 100 },
      { label: "GPU utilization", values: samples.map((sample) => sample.host.gpu_percent), maximum: 100 },
      { label: "Memory use", values: samples.map((sample) => {
        const used = sample.host.ram_used_bytes;
        const total = sample.host.ram_total_bytes;
        return used === null || total === null || total === 0 ? null : used * 100 / total;
      }), maximum: 100 },
      { label: "Network receive", values: samples.map((sample) => sample.host.net_rx_bps === null ? null : sample.host.net_rx_bps / 1_000_000), maximum: Math.max(10, ...samples.map((sample) => (sample.host.net_rx_bps ?? 0) / 1_000_000)) },
    ];
    const graphics = charts.map((chart) => {
      const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      svg.setAttribute("viewBox", "0 0 320 84");
      svg.setAttribute("width", "320");
      svg.setAttribute("height", "84");
      svg.style.width = "100%";
      svg.style.height = "84px";
      svg.setAttribute("role", "img");
      if (typeof Amaterasu.setLocalizedAttributeTemplate === "function") {
        Amaterasu.setLocalizedAttributeTemplate(
          svg,
          "aria-label",
          "{label} over recent measured samples",
          { label: hostMetricsTextMessage(chart.label) },
        );
      } else {
        svg.setAttribute("aria-label", `${chart.label} over recent measured samples`);
      }
      svg.classList.add("host-metric-chart");
      const axis = document.createElementNS(svg.namespaceURI, "path");
      axis.setAttribute("d", "M 0 78 H 320");
      axis.setAttribute("class", "host-metric-chart-axis");
      axis.setAttribute("stroke", "currentColor");
      axis.setAttribute("stroke-opacity", "0.3");
      svg.append(axis);
      const segments: Array<Array<{ x: number; y: number }>> = [];
      let segment: Array<{ x: number; y: number }> = [];
      chart.values.forEach((value, index) => {
        if (value === null) {
          if (segment.length) segments.push(segment);
          segment = [];
          return;
        }
        segment.push({
          x: (index / Math.max(chart.values.length - 1, 1)) * 320,
          y: 78 - Math.min(value / chart.maximum, 1) * 72,
        });
      });
      if (segment.length) segments.push(segment);
      for (const points of segments) {
        if (points.length === 1) {
          const point = points[0];
          if (!point) continue;
          const marker = document.createElementNS(svg.namespaceURI, "circle");
          marker.setAttribute("cx", String(point.x));
          marker.setAttribute("cy", String(point.y));
          marker.setAttribute("r", "3");
          marker.setAttribute("class", "host-metric-chart-line");
          svg.append(marker);
        } else {
          const line = document.createElementNS(svg.namespaceURI, "polyline");
          line.setAttribute("points", points.map((point) => `${point.x},${point.y}`).join(" "));
          line.setAttribute("class", "host-metric-chart-line");
          line.setAttribute("fill", "none");
          line.setAttribute("stroke", "currentColor");
          line.setAttribute("stroke-width", "2");
          svg.append(line);
        }
      }
      return el("div", "host-metric-chart-card", el("strong", "", chart.label), svg,
        el("small", "muted", hostMetricsTextLiteral(`${chart.values.filter((value) => value !== null).length} measured points`)));
    });
    const grid = el("div", "host-metric-chart-grid", ...graphics);
    grid.style.display = "grid";
    grid.style.gridTemplateColumns = "repeat(auto-fit, minmax(240px, 1fr))";
    grid.style.gap = "12px";
    return panel("Recent host charts", grid);
  }

  async function renderActiveSessions(context: PageContext): Promise<HTMLElement> {
    try {
      const status = await context.api.post<unknown>("history/history_status");
      if (!status || typeof status !== "object" || Array.isArray(status) ||
          (status as Record<string, unknown>).ok !== true)
        throw new Error("Session history availability is unavailable.");
      const runtime = status as Record<string, unknown>;
      if (runtime.effective_enabled !== true)
        return panel("Active streaming sessions",
          el("p", "muted", "Active stream metadata is unavailable while session history recording is disabled."));
    } catch (error) {
      return panel("Active streaming sessions", errorPanel(error));
    }
    const value = await context.api.post<unknown>("history/active_sessions", { limit: 16, offset: 0 });
    if (!value || typeof value !== "object" || Array.isArray(value) ||
        (value as Record<string, unknown>).ok !== true || !Array.isArray((value as Record<string, unknown>).sessions))
      throw new Error("Active session metrics reply is invalid.");
    const sessions = (value as { sessions: unknown[] }).sessions;
    if (sessions.length > 16) throw new Error("Active session metrics reply exceeds its limit.");
    const rows = sessions.map((entry) => {
      if (!entry || typeof entry !== "object" || Array.isArray(entry))
        throw new Error("Active session row is invalid.");
      const session = entry as Record<string, unknown>;
      const sessionId = session.session_id;
      const appName = session.app_name;
      const clientName = session.client_name;
      const protocol = session.protocol;
      if (typeof sessionId !== "string" || typeof appName !== "string" ||
          typeof clientName !== "string" || typeof protocol !== "string")
        throw new Error("Active session row is invalid.");
      return [appName || sessionId, clientName || protocol, protocol];
    });
    return panel("Active streaming sessions",
      rows.length ? table(["Application / session", "Client", "Protocol"], rows) :
        empty("No active streams", "Active stream metadata appears here while session history is enabled."));
  }

  function formatBytes(value: number): string {
    if (!Number.isFinite(value) || value < 0) return "Unavailable";
    const units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let amount = value;
    let index = 0;
    while (amount >= 1024 && index < units.length - 1) {
      amount /= 1024;
      index += 1;
    }
    return `${amount.toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
  }
}
