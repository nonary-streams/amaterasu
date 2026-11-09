namespace Amaterasu {
  // The host traces each input event and video frame through its plugins. This
  // view turns that trace into three answers: is the stream responsive, which
  // plugin is holding things up, and at which step the time goes.

  export interface PerformanceMetric {
    name: string;
    calls: number;
    totalUs: number;
    maxUs: number;
    p95Us: number | null;
  }
  export interface PerformanceTrackWindow {
    track: string;
    completed: number;
    slow: number;
    total: PerformanceMetric;
    steps: PerformanceMetric[];
  }
  export interface PerformanceWindow {
    observedUnixMs: number;
    windowUs: number;
    droppedRecords: number;
    tracks: PerformanceTrackWindow[];
    spans: PerformanceMetric[];
  }
  export interface PerformanceTrendPoint {
    observedUnixMs: number;
    tracks: { track: string; p95Us: number | null; slow: number }[];
  }
  export interface PerformanceOverlap { span: string; label: string | null; us: number }
  export interface PerformanceSlowFlow {
    observedUnixMs: number;
    track: string;
    totalUs: number;
    budgetUs: number;
    steps: { from: string; to: string; us: number }[];
    slowest: { from: string; to: string; us: number; percent: number; during: PerformanceOverlap[] } | null;
  }
  export interface PerformanceSlowSpan { observedUnixMs: number; span: string; label: string | null; us: number }
  export interface PerformanceTrace {
    collecting: boolean;
    windows: PerformanceWindow[];
    trend: PerformanceTrendPoint[];
    slowFlows: PerformanceSlowFlow[];
    slowSpans: PerformanceSlowSpan[];
  }

  export type PerformanceTone = "good" | "warn" | "bad" | "idle";
  export interface PerformanceVerdict { tone: PerformanceTone; label: string }
  export interface PerformanceStepView {
    id: string;
    label: string;
    meanUs: number;
    share: number;
    p95Us: number | null;
    maxUs: number;
    calls: number;
    /** Fixed color slot of this step, 1-5; 0 is the folded remainder. */
    slot: number;
  }
  export interface PerformanceTrackView {
    track: string;
    title: string;
    completed: number;
    slow: number;
    typicalUs: number | null;
    p95Us: number | null;
    worstUs: number;
    verdict: PerformanceVerdict;
    summary: string;
    steps: PerformanceStepView[];
    trend: { atUnixMs: number; us: number | null }[];
  }
  export interface PerformancePluginView {
    id: string;
    name: string;
    worstUs: number;
    totalUs: number;
    longCalls: number;
  }
  export interface PerformanceMoment {
    atUnixMs: number;
    headline: string;
    detail: string;
    totalUs: number;
    steps: { label: string; us: number; slot: number }[];
  }
  export interface PerformanceHostView {
    verdict: PerformanceVerdict;
    summary: string;
    typicalPassUs: number | null;
    worstHoldUs: number;
    passesPerSecond: number | null;
  }
  export interface PerformanceView {
    collecting: boolean;
    hasActivity: boolean;
    tracks: PerformanceTrackView[];
    host: PerformanceHostView;
    plugins: PerformancePluginView[];
    moments: PerformanceMoment[];
    droppedRecords: number;
  }

  /** The detailed windows cover about the last minute. */
  const RECENT_MS = 70_000;
  const MAX_ITEMS = 400;
  const MAX_SEGMENTS = 5;
  const MAX_PLUGINS = 8;
  const MAX_MOMENTS = 12;
  /** A plugin holding the host this long costs a frame at 120 Hz. */
  const NOTICEABLE_HOLD_US = 8_000;
  const LONG_HOLD_US = 16_000;

  const TRACK_TITLES: Record<string, string> = { input: "Input response", video: "Video delivery" };
  const KNOWN_STEPS: Record<string, [string, string][]> = {
    input: [
      ["socket.arrived>control.authenticated", "Waiting to be picked up"],
      ["control.authenticated>session.dequeued", "Reaching the stream session"],
      ["session.dequeued>session.submitted", "Waiting for the input plugin"],
      ["session.submitted>input.accepted", "Injecting into the system"],
      ["session.dequeued>session.consumed", "Handled by the session"],
      ["session.dequeued>session.coalesced", "Replaced by a newer sample"],
    ],
    video: [
      ["video.capture_started>video.encoder_submitted", "Capturing and preparing"],
      ["video.encoder_submitted>video.packet_queued", "Encoding"],
      ["video.packet_queued>session.packet_read", "Waiting for the stream session"],
      ["session.packet_read>transport.admitted", "Packetizing and sending"],
    ],
  };
  const PLUGIN_NAMES: Record<string, string> = {
    "@amaterasu.host-lifecycle": "Host",
    "amaterasu.application": "Applications",
    "amaterasu.audio": "Audio capture",
    "amaterasu.audio-encode": "Audio encoder",
    "amaterasu.authentication": "Sign-in",
    "amaterasu.capture-kms": "Screen capture (KMS)",
    "amaterasu.capture-pipewire": "Screen capture (PipeWire)",
    "amaterasu.capture-vkms": "Screen capture (VKMS)",
    "amaterasu.catalog": "App catalog",
    "amaterasu.configuration": "Configuration",
    "amaterasu.control-transport": "Control channel",
    "amaterasu.diagnostics": "Diagnostics",
    "amaterasu.display": "Display",
    "amaterasu.display-settings": "Display settings",
    "amaterasu.encode": "Video encoder",
    "amaterasu.extension-registry": "Extensions",
    "amaterasu.foreground-context": "Foreground app",
    "amaterasu.frame-limiter": "Frame limiter",
    "amaterasu.host-metrics": "Host metrics",
    "amaterasu.input": "Input",
    "amaterasu.launch": "App launcher",
    "amaterasu.library": "Library",
    "amaterasu.linux-session-video": "Video pipeline",
    "amaterasu.lutris": "Lutris library",
    "amaterasu.management": "Web interface",
    "amaterasu.media-transport": "Media transport",
    "amaterasu.network": "Network",
    "amaterasu.nvhttp": "Moonlight server",
    "amaterasu.pairing": "Pairing",
    "amaterasu.playnite": "Playnite library",
    "amaterasu.rtsp": "Stream setup",
    "amaterasu.rtx-hdr": "RTX HDR",
    "amaterasu.session": "Sessions",
    "amaterasu.session-history": "Session history",
    "amaterasu.session-lifecycle": "Session lifecycle",
    "amaterasu.state": "Saved state",
    "amaterasu.steam": "Steam library",
    "amaterasu.stream-session": "Stream session",
    "amaterasu.updater": "Updater",
    "amaterasu.video": "Video pipeline",
    "amaterasu.virtual-display": "Virtual display",
  };

  function perfRecord(value: unknown): Record<string, unknown> | null {
    return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
  }
  function perfList(value: unknown): Record<string, unknown>[] {
    if (!Array.isArray(value)) return [];
    const items: Record<string, unknown>[] = [];
    for (const item of value.slice(0, MAX_ITEMS)) {
      const record = perfRecord(item);
      if (record) items.push(record);
    }
    return items;
  }
  function perfCount(value: unknown): number {
    return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
  }
  function perfOptional(value: unknown): number | null {
    return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
  }
  function perfText(value: unknown): string {
    return typeof value === "string" ? value.slice(0, 160) : "";
  }
  function perfMetric(value: unknown): PerformanceMetric {
    const record = perfRecord(value) ?? {};
    return {
      name: perfText(record.stage),
      calls: perfCount(record.calls),
      totalUs: perfCount(record.total_us),
      maxUs: perfCount(record.max_us),
      p95Us: perfOptional(record.p95_upper_us),
    };
  }

  /** Returns null when the running host does not provide flow tracing. */
  export function parsePerformanceReply(reply: unknown): PerformanceTrace | null {
    const envelope = perfRecord(reply);
    if (!envelope || envelope.ok !== true) throw new Error("The host returned an invalid performance response.");
    if (envelope.supported !== true) return null;
    const trace = perfRecord(envelope.trace);
    if (!trace || trace.schema !== 1) throw new Error("The host returned an unsupported performance trace.");
    return {
      collecting: trace.collecting === true,
      windows: perfList(trace.windows).map((window) => ({
        observedUnixMs: perfCount(window.observed_unix_ms),
        windowUs: perfCount(window.window_us),
        droppedRecords: perfCount(window.dropped_records),
        tracks: perfList(window.tracks).map((track) => ({
          track: perfText(track.track),
          completed: perfCount(track.completed),
          slow: perfCount(track.slow),
          total: perfMetric(track.total),
          steps: perfList(track.steps).map(perfMetric),
        })),
        spans: perfList(window.spans).map(perfMetric),
      })),
      trend: perfList(trace.trend).map((point) => ({
        observedUnixMs: perfCount(point.observed_unix_ms),
        tracks: perfList(point.tracks).map((track) => ({
          track: perfText(track.track),
          p95Us: perfOptional(track.p95_upper_us),
          slow: perfCount(track.slow),
        })),
      })),
      slowFlows: perfList(trace.slow_flows).map((flow) => {
        const slowest = perfRecord(flow.slowest_step);
        return {
          observedUnixMs: perfCount(flow.observed_unix_ms),
          track: perfText(flow.track),
          totalUs: perfCount(flow.total_us),
          budgetUs: perfCount(flow.budget_us),
          steps: perfList(flow.steps).map((step) => ({
            from: perfText(step.from), to: perfText(step.to), us: perfCount(step.us),
          })),
          slowest: slowest && {
            from: perfText(slowest.from),
            to: perfText(slowest.to),
            us: perfCount(slowest.us),
            percent: Math.min(100, perfCount(slowest.percent)),
            during: perfList(slowest.during).map((span) => ({
              span: perfText(span.span),
              label: typeof span.label === "string" ? perfText(span.label) : null,
              us: perfCount(span.us),
            })),
          },
        };
      }),
      slowSpans: perfList(trace.slow_spans).map((span) => ({
        observedUnixMs: perfCount(span.observed_unix_ms),
        span: perfText(span.span),
        label: typeof span.label === "string" ? perfText(span.label) : null,
        us: perfCount(span.us),
      })),
    };
  }

  export function formatPerformanceDuration(us: number): string {
    if (us < 100) return "under 0.1 ms";
    if (us < 10_000) return `${(us / 1000).toFixed(1)} ms`;
    if (us < 1_000_000) return `${Math.round(us / 1000)} ms`;
    return `${(us / 1_000_000).toFixed(1)} s`;
  }

  export function performancePluginName(id: string): string {
    const known = PLUGIN_NAMES[id];
    if (known) return known;
    const bare = id.replace(/^@?amaterasu\./, "").replace(/[-_.]+/g, " ").trim();
    return bare ? bare.charAt(0).toUpperCase() + bare.slice(1) : "Unknown plugin";
  }

  function stepLabel(track: string, id: string): { label: string; slot: number } {
    const known = KNOWN_STEPS[track] ?? [];
    const index = known.findIndex(([step]) => step === id);
    if (index >= 0) return { label: known[index]![1], slot: Math.min(index + 1, MAX_SEGMENTS) };
    return { label: id.replace(">", " → "), slot: 0 };
  }

  function trackVerdict(completed: number, slow: number): PerformanceVerdict {
    if (!completed) return { tone: "idle", label: "No activity" };
    if (!slow) return { tone: "good", label: "Smooth" };
    return slow * 100 <= completed
      ? { tone: "warn", label: "Occasional delays" }
      : { tone: "bad", label: "Frequent delays" };
  }

  function trackView(track: string, windows: PerformanceWindow[], trend: PerformanceTrendPoint[]): PerformanceTrackView {
    let completed = 0;
    let slow = 0;
    let totalUs = 0;
    let calls = 0;
    let worstUs = 0;
    let p95Us: number | null = null;
    const steps = new Map<string, { calls: number; totalUs: number; maxUs: number; p95Us: number | null }>();
    for (const window of windows) {
      for (const item of window.tracks) {
        if (item.track !== track) continue;
        completed += item.completed;
        slow += item.slow;
        totalUs += item.total.totalUs;
        calls += item.total.calls;
        worstUs = Math.max(worstUs, item.total.maxUs);
        if (item.total.p95Us !== null) p95Us = Math.max(p95Us ?? 0, item.total.p95Us);
        for (const step of item.steps) {
          const entry = steps.get(step.name) ?? { calls: 0, totalUs: 0, maxUs: 0, p95Us: null };
          entry.calls += step.calls;
          entry.totalUs += step.totalUs;
          entry.maxUs = Math.max(entry.maxUs, step.maxUs);
          if (step.p95Us !== null) entry.p95Us = Math.max(entry.p95Us ?? 0, step.p95Us);
          steps.set(step.name, entry);
        }
      }
    }
    const stepTotal = [...steps.values()].reduce((sum, step) => sum + step.totalUs, 0);
    const views: PerformanceStepView[] = [...steps].map(([id, step]) => ({
      id,
      ...stepLabel(track, id),
      meanUs: step.calls ? step.totalUs / step.calls : 0,
      share: stepTotal ? step.totalUs / stepTotal : 0,
      p95Us: step.p95Us,
      maxUs: step.maxUs,
      calls: step.calls,
    }));
    // Known steps keep their place in the path; the rest follow by time spent.
    views.sort((a, b) => (a.slot || 99) - (b.slot || 99) || b.share - a.share || a.id.localeCompare(b.id));
    const title = TRACK_TITLES[track] ?? `${track.charAt(0).toUpperCase()}${track.slice(1)} flow`;
    const typicalUs = calls ? totalUs / calls : null;
    const verdict = trackVerdict(completed, slow);
    const subject = track === "input" ? "input event" : track === "video" ? "frame" : "item";
    const summary = typicalUs === null
      ? `No ${subject}s passed through the host in the last minute.`
      : slow
        ? `${slow.toLocaleString()} of ${completed.toLocaleString()} ${subject}s took longer than they should have.`
        : `A typical ${subject} spends ${formatPerformanceDuration(typicalUs)} inside the host.`;
    return {
      track, title, completed, slow, typicalUs, p95Us, worstUs, verdict, summary,
      steps: views,
      trend: trend.map((point) => ({
        atUnixMs: point.observedUnixMs,
        us: point.tracks.find((item) => item.track === track)?.p95Us ?? null,
      })),
    };
  }

  function spanParts(name: string): { kind: string; label: string | null } {
    const separator = name.indexOf(":");
    return separator < 0
      ? { kind: name, label: null }
      : { kind: name.slice(0, separator), label: name.slice(separator + 1) };
  }
  function holdsHost(kind: string): boolean {
    return kind === "host.tick" || kind === "host.health";
  }

  function momentFromFlow(flow: PerformanceSlowFlow): PerformanceMoment {
    const subject = flow.track === "input"
      ? "An input event took"
      : flow.track === "video" ? "A video frame took" : `A ${flow.track} item took`;
    const destination = flow.track === "input" ? "to reach the system" : flow.track === "video" ? "to leave the host" : "to finish";
    let detail = "";
    if (flow.slowest) {
      const step = stepLabel(flow.track, `${flow.slowest.from}>${flow.slowest.to}`).label;
      detail = `Most of it (${Math.round(flow.slowest.percent)}%) was “${step.toLowerCase()}”.`;
      const cause = flow.slowest.during[0];
      if (cause && holdsHost(cause.span) && cause.label)
        detail += ` ${performancePluginName(cause.label)} was busy for ${formatPerformanceDuration(cause.us)} at that moment.`;
      else if (cause?.span === "host.wait")
        detail += " The host was idle between checks at that moment, so nothing was blocking it.";
    }
    return {
      atUnixMs: flow.observedUnixMs,
      headline: `${subject} ${formatPerformanceDuration(flow.totalUs)} ${destination}`,
      detail,
      totalUs: flow.totalUs,
      steps: flow.steps.map((step) => ({ ...stepLabel(flow.track, `${step.from}>${step.to}`), us: step.us })),
    };
  }

  function momentFromSpan(span: PerformanceSlowSpan): PerformanceMoment {
    const pausesHost = holdsHost(span.span) && span.label;
    return {
      atUnixMs: span.observedUnixMs,
      headline: pausesHost
        ? `${performancePluginName(span.label!)} held the host for ${formatPerformanceDuration(span.us)}`
        : `${span.label ? `${span.span} (${span.label})` : span.span} took ${formatPerformanceDuration(span.us)}`,
      detail: pausesHost ? "Input and video for every stream waited behind it." : "",
      totalUs: span.us,
      steps: [],
    };
  }

  export function performanceView(trace: PerformanceTrace, nowUnixMs: number): PerformanceView {
    const recent = trace.windows.filter((window) => nowUnixMs - window.observedUnixMs <= RECENT_MS);
    const trackNames = new Set<string>(["input", "video"]);
    for (const window of recent) for (const track of window.tracks) if (track.track) trackNames.add(track.track);
    const tracks = [...trackNames].slice(0, 6).map((track) => trackView(track, recent, trace.trend));

    const plugins = new Map<string, PerformancePluginView>();
    let passCalls = 0;
    let passTotalUs = 0;
    let windowUs = 0;
    for (const window of recent) {
      windowUs += window.windowUs;
      for (const span of window.spans) {
        const { kind, label } = spanParts(span.name);
        if (kind === "host.pass") {
          passCalls += span.calls;
          passTotalUs += span.totalUs;
        }
        if (!holdsHost(kind) || !label) continue;
        const entry = plugins.get(label) ?? {
          id: label, name: performancePluginName(label), worstUs: 0, totalUs: 0, longCalls: 0,
        };
        entry.worstUs = Math.max(entry.worstUs, span.maxUs);
        entry.totalUs += span.totalUs;
        entry.longCalls += span.calls;
        plugins.set(label, entry);
      }
    }
    const ranked = [...plugins.values()]
      .sort((a, b) => b.worstUs - a.worstUs || b.totalUs - a.totalUs || a.id.localeCompare(b.id))
      .slice(0, MAX_PLUGINS);
    const worstHoldUs = ranked[0]?.worstUs ?? 0;
    const hostVerdict: PerformanceVerdict = !passCalls
      ? { tone: "idle", label: "No data yet" }
      : worstHoldUs >= LONG_HOLD_US
        ? { tone: "bad", label: "Long pauses" }
        : worstHoldUs >= NOTICEABLE_HOLD_US
          ? { tone: "warn", label: "Brief pauses" }
          : { tone: "good", label: "Responsive" };
    const culprit = ranked[0];
    const host: PerformanceHostView = {
      verdict: hostVerdict,
      typicalPassUs: passCalls ? passTotalUs / passCalls : null,
      worstHoldUs,
      passesPerSecond: passCalls && windowUs ? passCalls / (windowUs / 1_000_000) : null,
      summary: !passCalls
        ? "Waiting for the host to report its first measurements."
        : culprit && worstHoldUs >= NOTICEABLE_HOLD_US
          ? `${culprit.name} paused everything for up to ${formatPerformanceDuration(worstHoldUs)}.`
          : "No plugin held up the others for long enough to notice.",
    };

    const moments = [
      ...trace.slowFlows.map(momentFromFlow),
      ...trace.slowSpans.map(momentFromSpan),
    ].sort((a, b) => b.atUnixMs - a.atUnixMs).slice(0, MAX_MOMENTS);

    return {
      collecting: trace.collecting,
      hasActivity: recent.length > 0,
      tracks,
      host,
      plugins: ranked,
      moments,
      droppedRecords: recent.reduce((sum, window) => sum + window.droppedRecords, 0),
    };
  }

  // ---- rendering -------------------------------------------------------------

  const VERDICT_MARK: Record<PerformanceTone, string> = { good: "✓", warn: "▲", bad: "✕", idle: "–" };

  function verdictBadge(verdict: PerformanceVerdict): HTMLElement {
    const tone = verdict.tone === "good" ? "good" : verdict.tone === "bad" ? "bad" : verdict.tone === "warn" ? "warn" : "";
    const mark = el("span", "perf-verdict-mark", literalText(VERDICT_MARK[verdict.tone]));
    mark.setAttribute("aria-hidden", "true");
    return el("span", `badge perf-verdict ${tone}`, mark, verdict.label);
  }

  interface PerformanceTip { attach(mark: HTMLElement | SVGElement, lines: () => string[]): void; element: HTMLElement }

  /** One tooltip for every mark. It repeats what the tables already show. */
  function performanceTip(root: HTMLElement): PerformanceTip {
    const element = el("div", "perf-tooltip");
    element.setAttribute("role", "tooltip");
    element.hidden = true;
    const show = (mark: Element, lines: string[]) => {
      element.replaceChildren(...lines.map((line, index) => el(index ? "span" : "strong", "", literalText(line))));
      element.hidden = false;
      if (typeof mark.getBoundingClientRect !== "function" || typeof root.getBoundingClientRect !== "function") return;
      const markBox = mark.getBoundingClientRect();
      const rootBox = root.getBoundingClientRect();
      element.style.left = `${Math.max(0, markBox.left - rootBox.left)}px`;
      element.style.top = `${markBox.bottom - rootBox.top + 6}px`;
    };
    return {
      element,
      attach(mark, lines) {
        mark.setAttribute("tabindex", "0");
        for (const name of ["pointerenter", "focus"]) mark.addEventListener(name, () => show(mark, lines()));
        for (const name of ["pointerleave", "blur"]) mark.addEventListener(name, () => { element.hidden = true; });
      },
    };
  }

  function perfSvg<K extends keyof SVGElementTagNameMap>(tag: K, attributes: Record<string, string>): SVGElementTagNameMap[K] {
    const node = document.createElementNS("http://www.w3.org/2000/svg", tag);
    for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, value);
    return node;
  }

  /** The last half hour of a track's 95th percentile, newest at the right. */
  function trendSparkline(view: PerformanceTrackView, tip: PerformanceTip): HTMLElement | null {
    const points = view.trend.filter((point) => point.us !== null) as { atUnixMs: number; us: number }[];
    if (points.length < 2 || typeof document.createElementNS !== "function") return null;
    const width = 160;
    const height = 32;
    const inset = 5;
    const peak = Math.max(...points.map((point) => point.us), 1);
    // A fixed floor for the scale keeps small, healthy variation looking small.
    const ceiling = Math.max(peak, LONG_HOLD_US);
    const x = (index: number) => inset + index * (width - 2 * inset) / (points.length - 1);
    const y = (us: number) => height - inset - us * (height - 2 * inset) / ceiling;
    const svg = perfSvg("svg", { viewBox: `0 0 ${width} ${height}`, class: "perf-sparkline", role: "img" });
    const last = points[points.length - 1]!;
    svg.setAttribute("aria-label",
      `${view.title} over the last ${Math.round((last.atUnixMs - points[0]!.atUnixMs) / 60_000)} minutes: ` +
      `95% within ${formatPerformanceDuration(last.us)} now, peak ${formatPerformanceDuration(peak)}.`);
    svg.append(
      perfSvg("polyline", {
        points: points.map((point, index) => `${x(index).toFixed(1)},${y(point.us).toFixed(1)}`).join(" "),
        class: "perf-sparkline-line",
      }),
      perfSvg("circle", {
        cx: x(points.length - 1).toFixed(1), cy: y(last.us).toFixed(1), r: "4", class: "perf-sparkline-now",
      }),
    );
    const holder = el("div", "perf-sparkline-holder");
    holder.append(svg);
    tip.attach(holder, () => [
      `95% within ${formatPerformanceDuration(last.us)} now`,
      `Peak in this period: ${formatPerformanceDuration(peak)}`,
    ]);
    return holder;
  }

  function statTile(
    label: string, value: string, caption: string, verdict: PerformanceVerdict, summary: string,
    extra: (HTMLElement | null)[],
  ): HTMLElement {
    return el("article", "perf-tile",
      el("div", "perf-tile-head", el("h3", "", label), verdictBadge(verdict)),
      el("p", "perf-tile-value", literalText(value), el("span", "perf-tile-caption", caption)),
      el("p", "muted perf-tile-summary", literalText(summary)),
      ...extra,
    );
  }

  function pluginRows(view: PerformanceView, tip: PerformanceTip): HTMLElement {
    if (!view.plugins.length) {
      return el("p", "muted", view.host.typicalPassUs === null
        ? "No measurements yet."
        : "No plugin has held the host for more than a quarter of a millisecond in the last minute.");
    }
    // One scale for every row, never shorter than a frame so small values stay small.
    const scale = Math.max(NOTICEABLE_HOLD_US, ...view.plugins.map((plugin) => plugin.worstUs));
    const list = el("ol", "perf-plugins");
    for (const plugin of view.plugins) {
      const bar = el("span", "perf-bar");
      bar.style.width = `${Math.max(1, plugin.worstUs * 100 / scale).toFixed(1)}%`;
      const track = el("span", "perf-bar-track", bar);
      const figure = `${formatPerformanceDuration(plugin.worstUs)} at worst`;
      // Every row has the same four cells, so all bars share one scale.
      const row = el("li", "perf-plugin",
        el("span", "perf-plugin-name", literalText(plugin.name)),
        track,
        el("span", "perf-plugin-figure", literalText(figure)),
        el("span", "perf-plugin-flag", plugin.worstUs >= NOTICEABLE_HOLD_US
          ? verdictBadge(plugin.worstUs >= LONG_HOLD_US
            ? { tone: "bad", label: "Long pause" } : { tone: "warn", label: "Noticeable" })
          : null),
      );
      row.setAttribute("aria-label",
        `${plugin.name}: held the host for ${figure}, ${plugin.longCalls} longer checks in the last minute.`);
      tip.attach(row, () => [
        `${plugin.name} · ${figure}`,
        `${plugin.longCalls} checks over 0.25 ms, ${formatPerformanceDuration(plugin.totalUs)} in total`,
        plugin.id,
      ]);
      list.append(row);
    }
    return list;
  }

  /** Folds steps past the fifth into one remainder so the bar stays readable. */
  function segmentsOf(steps: PerformanceStepView[]): PerformanceStepView[] {
    const shown = steps.filter((step) => step.share > 0);
    if (shown.length <= MAX_SEGMENTS + 1) return shown;
    const kept = shown.slice(0, MAX_SEGMENTS);
    const rest = shown.slice(MAX_SEGMENTS);
    return [...kept, {
      id: "other", label: "Other steps", slot: 0,
      meanUs: rest.reduce((sum, step) => sum + step.meanUs, 0),
      share: rest.reduce((sum, step) => sum + step.share, 0),
      p95Us: null,
      maxUs: Math.max(...rest.map((step) => step.maxUs)),
      calls: Math.max(...rest.map((step) => step.calls)),
    }];
  }

  /** A share is printed only where its block can hold it; it is never clipped. */
  function fitSegmentLabels(bar: HTMLElement): void {
    if (typeof ResizeObserver === "undefined") return;
    const labels = new Map<Element, string>();
    for (const segment of Array.from(bar.children)) labels.set(segment, segment.textContent ?? "");
    new ResizeObserver(() => {
      for (const [segment, label] of labels) {
        if (!label) continue;
        segment.textContent = label;
        if (segment.scrollWidth > segment.clientWidth) segment.textContent = "";
      }
    }).observe(bar);
  }

  function stepBreakdown(view: PerformanceTrackView, tip: PerformanceTip): HTMLElement {
    const heading = el("h4", "", view.title);
    if (!view.steps.length || view.typicalUs === null)
      return el("div", "perf-breakdown", heading, el("p", "muted", literalText(view.summary)));
    const segments = segmentsOf(view.steps);
    // Unknown steps take the free slots in a stable order; colour follows the step.
    const used = new Set(segments.map((step) => step.slot).filter((slot) => slot > 0));
    let free = 1;
    const slotOf = new Map<string, number>();
    for (const step of segments) {
      let slot = step.slot;
      if (!slot && step.id !== "other") {
        while (used.has(free) && free <= MAX_SEGMENTS) free += 1;
        if (free <= MAX_SEGMENTS) { slot = free; used.add(free); }
      }
      slotOf.set(step.id, slot);
    }
    const bar = el("div", "perf-segments");
    bar.setAttribute("role", "img");
    bar.setAttribute("aria-label", `${view.title} by step: ${segments
      .map((step) => `${step.label} ${Math.round(step.share * 100)}%`).join(", ")}.`);
    const legend = el("ul", "perf-legend");
    for (const step of segments) {
      const slot = slotOf.get(step.id) ?? 0;
      const percent = Math.round(step.share * 100);
      const segment = el("span", `perf-segment perf-slot-${slot}`, percent >= 9 ? literalText(`${percent}%`) : null);
      segment.style.flexGrow = String(Math.max(step.share, 0.004));
      tip.attach(segment, () => [
        `${step.label} · ${formatPerformanceDuration(step.meanUs)} typical`,
        `${percent}% of the time inside the host`,
      ]);
      bar.append(segment);
      legend.append(el("li", "", el("span", `perf-swatch perf-slot-${slot}`), literalText(step.label)));
    }
    fitSegmentLabels(bar);
    const figures = `Typical ${formatPerformanceDuration(view.typicalUs)}` +
      (view.p95Us === null ? "" : ` · 95% within ${formatPerformanceDuration(view.p95Us)}`) +
      ` · worst ${formatPerformanceDuration(view.worstUs)}`;
    const details = el("details", "perf-details",
      el("summary", "", "Step details"),
      table(["Step", "Share", "Typical", "95% within", "Worst", "Samples", "Trace name"], view.steps.map((step) => [
        literalText(step.label),
        literalText(`${Math.round(step.share * 100)}%`),
        literalText(formatPerformanceDuration(step.meanUs)),
        literalText(step.p95Us === null ? "—" : formatPerformanceDuration(step.p95Us)),
        literalText(formatPerformanceDuration(step.maxUs)),
        literalText(String(step.calls)),
        el("code", "", literalText(step.id)),
      ])),
    );
    return el("div", "perf-breakdown", heading, el("p", "muted perf-figures", literalText(figures)), bar, legend, details);
  }

  function momentRows(view: PerformanceView): HTMLElement {
    if (!view.moments.length)
      return el("p", "muted", "Nothing slow has been recorded since the host started tracing.");
    const list = el("ol", "perf-moments");
    for (const moment of view.moments) {
      const time = el("time", "muted", literalText(new Date(moment.atUnixMs).toLocaleTimeString()));
      time.dateTime = new Date(moment.atUnixMs).toISOString();
      const strip = el("div", "perf-strip");
      strip.setAttribute("aria-hidden", "true");
      for (const step of moment.steps) {
        if (!step.us) continue;
        const part = el("span", `perf-strip-part perf-slot-${step.slot}`);
        part.style.flexGrow = String(step.us);
        strip.append(part);
      }
      list.append(el("li", "perf-moment",
        time,
        el("div", "perf-moment-body",
          el("strong", "", literalText(moment.headline)),
          moment.detail ? el("p", "muted", literalText(moment.detail)) : null,
          moment.steps.length ? strip : null,
          moment.steps.length
            ? el("details", "perf-details",
              el("summary", "", "Steps"),
              table(["Step", "Time"], moment.steps.map((step) => [
                literalText(step.label), literalText(formatPerformanceDuration(step.us)),
              ])))
            : null,
        ),
      ));
    }
    return list;
  }

  export function performanceContent(view: PerformanceView): HTMLElement {
    const root = el("div", "perf");
    const tip = performanceTip(root);
    const tiles = el("div", "perf-tiles");
    for (const track of view.tracks.slice(0, 2)) {
      tiles.append(statTile(
        track.title,
        track.typicalUs === null ? "—" : formatPerformanceDuration(track.typicalUs),
        "typical time inside the host",
        track.verdict,
        track.summary,
        [trendSparkline(track, tip)],
      ));
    }
    tiles.append(statTile(
      "Host responsiveness",
      view.host.typicalPassUs === null ? "—" : formatPerformanceDuration(view.host.typicalPassUs),
      "typical round of plugin checks",
      view.host.verdict,
      view.host.summary,
      [view.host.passesPerSecond === null ? null : el("p", "muted perf-tile-note",
        literalText(`The host checks every plugin about ${Math.round(view.host.passesPerSecond)} times a second.`))],
    ));
    if (view.droppedRecords)
      root.append(el("p", "notice", literalText(`${view.droppedRecords} measurements were dropped because the host was too busy to keep them; the figures below may understate the load.`)));
    root.append(
      tiles,
      el("section", "perf-section",
        el("h3", "", "What is holding things up"),
        el("p", "muted", "Every plugin shares one host loop. This is the longest each one kept the others waiting in the last minute."),
        pluginRows(view, tip)),
      el("section", "perf-section",
        el("h3", "", "Where the time goes"),
        el("p", "muted", "Average time at each step inside the host. Network, client and display time are not included."),
        ...view.tracks.map((track) => stepBreakdown(track, tip))),
      el("section", "perf-section",
        el("h3", "", "Recent slow moments"),
        momentRows(view)),
      tip.element,
    );
    return root;
  }

  /** Renders the performance panel and keeps it current while it is visible. */
  export function renderPerformance(context: PageContext, target: HTMLElement): () => Promise<void> {
    const REFRESH_MS = 10_000;
    const body = el("div", "perf-body", el("p", "muted", "Loading performance measurements…"));
    const status = el("p", "muted perf-status");
    status.setAttribute("role", "status");
    target.replaceChildren(panel(
      "Performance",
      el("p", "muted", "How quickly the host passes input and video through its plugins, and what slows it down."),
      body,
      status,
    ));
    let generation = 0;
    let timer: number | null = null;
    function schedule(): void {
      if (timer !== null) window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        timer = null;
        // Leaving the page ends the refresh loop.
        if (!target.isConnected) return;
        if (document.visibilityState === "visible") void load();
        else schedule();
      }, REFRESH_MS);
    }
    async function load(): Promise<void> {
      const current = ++generation;
      // Keep the previous figures on screen while the next ones load.
      body.classList?.add("perf-refreshing");
      try {
        const trace = parsePerformanceReply(await context.api.post<unknown>("diagnostics/performance"));
        if (current !== generation || !target.isConnected) return;
        if (!trace) {
          body.replaceChildren(empty(
            "Performance tracing unavailable",
            "The running host does not record flow timing yet. Install a host update that includes it.",
          ));
          status.textContent = "";
          return;
        }
        body.replaceChildren(performanceContent(performanceView(trace, Date.now())));
        status.textContent = trace.collecting
          ? `Updated ${new Date().toLocaleTimeString()} · refreshes every 10 seconds`
          : "No plugin has started tracing yet.";
      } catch (error) {
        if (current === generation && target.isConnected) body.replaceChildren(errorPanel(error, load));
      } finally {
        if (current === generation) {
          body.classList?.remove("perf-refreshing");
          schedule();
        }
      }
    }
    void load();
    return load;
  }
}
