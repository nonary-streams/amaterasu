namespace Amaterasu {
  // The performance panel of the Diagnostics page. It reads the runtime's
  // compact report (docs/flow-tracing.md) and shows where time goes inside the
  // host: per stage for input and video, and per plugin call on the host loop.

  /** One stage or call over the reported period. The percentile is an upper bound. */
  export interface PerformanceMetric {
    stage: string;
    calls: number;
    meanUs: number | null;
    p95Us: number;
    maxUs: number;
    totalUs: number;
  }
  export interface PerformanceTrack {
    track: string;
    completed: number;
    slow: number;
    abandoned: number;
    budgetUs: number;
    total: PerformanceMetric;
    steps: PerformanceMetric[];
  }
  export interface PerformanceTrendTrack { budgetUs: number | null; maxUs: (number | null)[] }
  /** Columns of one value per ten-second window, oldest first. */
  export interface PerformanceTrend {
    newestUnixMs: number;
    ageS: number[];
    tracks: Record<string, PerformanceTrendTrack>;
    hostPassMaxUs: (number | null)[];
  }
  export interface PerformanceHold { span: string; label: string | null; us: number }
  /**
   * Something that exceeded its limit, or a call that held a thread too long.
   * A run of them with one cause is a single moment from `observedUnixMs` to
   * `untilUnixMs`; `us` is the worst of its `count` members.
   */
  export interface PerformanceMoment {
    kind: "flow" | "span";
    observedUnixMs: number;
    untilUnixMs: number;
    count: number;
    us: number;
    track: string;
    step: string;
    stepUs: number;
    heldBy: PerformanceHold | null;
    span: string;
    label: string | null;
  }
  export interface PerformanceReport {
    collecting: boolean;
    /** Zero until the first window has been collected. */
    periodUs: number;
    droppedRecords: number;
    tracks: PerformanceTrack[];
    hostPass: PerformanceMetric | null;
    calls: PerformanceMetric[];
    callsOmitted: number;
    trend: PerformanceTrend | null;
    moments: PerformanceMoment[];
  }

  const REPORT_SCHEMA = 2;
  /** A call that holds the host loop this long is reported on its own. */
  const LONG_CALL_US = 8_000;
  const TREND_SLOTS = 180;
  const TREND_SLOT_S = 10;
  const LISTED_CALLS = 10;
  const LISTED_MOMENTS = 12;

  const TRACKS: Record<string, { title: string; one: string; many: string }> = {
    input: { title: "Input", one: "input event", many: "input events" },
    video: { title: "Video", one: "video frame", many: "video frames" },
  };
  const STAGES: Record<string, string> = {
    "socket.arrived>control.authenticated": "Waiting to be read from the network",
    "control.authenticated>session.dequeued": "Reaching the stream session",
    "session.dequeued>session.submitted": "Preparing the event",
    "session.submitted>input.accepted": "Injecting into the system",
    "session.dequeued>session.coalesced": "Replaced by a newer sample",
    "video.capture_started>video.encoder_submitted": "Capturing and converting",
    "video.encoder_submitted>video.packet_queued": "Encoding",
    "video.packet_queued>session.packet_read": "Waiting for the stream session",
    "session.packet_read>transport.admitted": "Packetizing and sending",
  };
  const PLUGINS: Record<string, string> = {
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

  // Reading the report. Anything missing reads as zero or absent, so a newer
  // host that adds fields never breaks an older page.

  function reportObject(value: unknown): Record<string, unknown> | null {
    return typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : null;
  }
  function reportList(value: unknown): unknown[] {
    return Array.isArray(value) ? value : [];
  }
  function reportNumber(value: unknown): number {
    return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
  }
  function reportOptional(value: unknown): number | null {
    return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
  }
  function reportText(value: unknown): string {
    return typeof value === "string" ? value : "";
  }
  function reportMetric(value: unknown): PerformanceMetric {
    const metric = reportObject(value) ?? {};
    return {
      stage: reportText(metric.stage),
      calls: reportNumber(metric.calls),
      meanUs: reportOptional(metric.mean_us),
      p95Us: reportNumber(metric.p95_upper_us),
      maxUs: reportNumber(metric.max_us),
      totalUs: reportNumber(metric.total_us),
    };
  }
  function reportTrack(value: unknown): PerformanceTrack {
    const track = reportObject(value) ?? {};
    return {
      track: reportText(track.track),
      completed: reportNumber(track.completed),
      slow: reportNumber(track.slow),
      abandoned: reportNumber(track.abandoned),
      budgetUs: reportNumber(track.budget_us),
      total: reportMetric(track.total),
      steps: reportList(track.steps).map(reportMetric),
    };
  }
  function reportTrend(value: unknown): PerformanceTrend | null {
    const trend = reportObject(value);
    if (!trend) return null;
    const tracks: Record<string, PerformanceTrendTrack> = {};
    for (const [name, columns] of Object.entries(reportObject(trend.tracks) ?? {})) {
      const track = reportObject(columns) ?? {};
      tracks[name] = { budgetUs: reportOptional(track.budget_us), maxUs: reportList(track.max_us).map(reportOptional) };
    }
    return {
      newestUnixMs: reportNumber(trend.newest_unix_ms),
      ageS: reportList(trend.age_s).map(reportNumber),
      tracks,
      hostPassMaxUs: reportList(trend.host_pass_max_us).map(reportOptional),
    };
  }
  function reportMoment(value: unknown): PerformanceMoment {
    const moment = reportObject(value) ?? {};
    const hold = reportObject(moment.held_by);
    return {
      kind: moment.kind === "span" ? "span" : "flow",
      observedUnixMs: reportNumber(moment.observed_unix_ms),
      untilUnixMs: Math.max(reportNumber(moment.observed_unix_ms), reportNumber(moment.until_unix_ms)),
      count: Math.max(1, reportNumber(moment.count)),
      us: reportNumber(moment.us),
      track: reportText(moment.track),
      step: reportText(moment.step),
      stepUs: reportNumber(moment.step_us),
      heldBy: hold ? { span: reportText(hold.span), label: reportText(hold.label) || null, us: reportNumber(hold.us) } : null,
      span: reportText(moment.span),
      label: reportText(moment.label) || null,
    };
  }

  /**
   * The report of a management reply, or null when the running host does not
   * record flow timing. A reply this page cannot read is an error, not an
   * empty report: an empty report would look like a healthy host.
   */
  export function parsePerformanceReply(reply: unknown): PerformanceReport | null {
    const body = reportObject(reply);
    if (!body || body.ok !== true) throw new Error("The host did not return a performance report.");
    if (body.supported !== true) return null;
    const trace = reportObject(body.trace);
    if (!trace || trace.schema !== REPORT_SCHEMA) throw new Error("The host returned a performance report this page cannot read.");
    const recent = reportObject(trace.recent) ?? {};
    const host = reportObject(recent.host) ?? {};
    return {
      collecting: trace.collecting === true,
      periodUs: reportNumber(recent.window_us),
      droppedRecords: reportNumber(recent.dropped_records),
      tracks: reportList(recent.tracks).map(reportTrack),
      hostPass: reportObject(host.pass) ? reportMetric(host.pass) : null,
      calls: reportList(recent.spans).map(reportMetric),
      callsOmitted: reportNumber(recent.spans_omitted),
      trend: reportTrend(trace.trend),
      moments: reportList(trace.slow).map(reportMoment),
    };
  }

  // Wording.

  export function formatPerformanceDuration(us: number): string {
    if (us < 10) return "<0.01 ms";
    if (us < 1_000) return `${(us / 1000).toFixed(2)} ms`;
    if (us < 100_000) return `${(us / 1000).toFixed(1)} ms`;
    if (us < 1_000_000) return `${Math.round(us / 1000)} ms`;
    return `${(us / 1_000_000).toFixed(1)} s`;
  }
  /** A limit is a round number; it reads better without a trailing zero. */
  function formatLimit(us: number): string {
    return us % 1000 === 0 ? `${us / 1000} ms` : formatPerformanceDuration(us);
  }
  function formatCount(value: number): string {
    return Math.round(value).toLocaleString("en-US");
  }
  function formatClock(unixMs: number): string {
    return new Date(unixMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  }
  export function performancePluginName(id: string): string {
    const known = PLUGINS[id];
    if (known) return known;
    const bare = id.replace(/^@?amaterasu\./, "").replace(/[-_.]+/g, " ").trim();
    return bare ? bare.charAt(0).toUpperCase() + bare.slice(1) : "Unknown plugin";
  }
  function stageName(id: string): string {
    return STAGES[id] ?? id.replace(">", " to ");
  }
  function trackWords(track: string): { title: string; one: string; many: string } {
    return TRACKS[track] ?? { title: track, one: `${track} item`, many: `${track} items` };
  }
  /** `host.tick:amaterasu.pairing` is the pairing plugin's turn on the host loop. */
  function callParts(stage: string): { kind: string; plugin: string | null } {
    const split = stage.indexOf(":");
    return split < 0 ? { kind: stage, plugin: null } : { kind: stage.slice(0, split), plugin: stage.slice(split + 1) };
  }
  function holdsHostLoop(kind: string): boolean {
    return kind === "host.tick" || kind === "host.health";
  }

  /** What happened in one slow moment, as one sentence. */
  export function performanceMomentText(moment: PerformanceMoment): string {
    if (moment.kind === "span") {
      const plugin = moment.label ? performancePluginName(moment.label) : "A plugin";
      const times = moment.count > 1 ? ` ${formatCount(moment.count)} times` : "";
      return holdsHostLoop(moment.span)
        ? `${plugin} held the host loop${times}, delaying every other plugin.`
        : `${plugin} spent this long in ${moment.span}${times}.`;
    }
    const words = trackWords(moment.track);
    const subject = moment.count > 1 ? `${formatCount(moment.count)} ${words.many}` : `One ${words.one}`;
    const stage = `${stageName(moment.step).toLowerCase()} (${formatPerformanceDuration(moment.stepUs)})`;
    const hold = moment.heldBy;
    if (!hold) return `${subject}: slowest at ${stage}.`;
    const holder = hold.label ? performancePluginName(hold.label) : hold.span;
    return `${subject}: slowest at ${stage}, during a ${formatPerformanceDuration(hold.us)} call by ${holder}.`;
  }

  // Building the panel.

  function durationCell(us: number | null, limitUs = 0): HTMLElement {
    const cell = el("td", limitUs && us !== null && us >= limitUs ? "perf-num perf-over" : "perf-num");
    cell.textContent = us === null ? "–" : formatPerformanceDuration(us);
    return cell;
  }
  function countCell(value: number): HTMLElement {
    const cell = el("td", "perf-num");
    cell.textContent = formatCount(value);
    return cell;
  }
  function dataTable(headers: [string, boolean][], rows: HTMLElement[]): HTMLElement {
    const head = el("tr");
    for (const [name, numeric] of headers) {
      const cell = el("th", numeric ? "perf-num" : "", name);
      cell.scope = "col";
      head.append(cell);
    }
    return el("div", "table-scroll", el("table", "perf-table", el("thead", "", head), el("tbody", "", ...rows)));
  }
  /** A proportion drawn as a thin bar with its figure. */
  function proportion(fraction: number, text: string): HTMLElement {
    const bar = el("span", "perf-bar");
    bar.style.width = `${Math.max(0, Math.min(1, fraction)) * 100}%`;
    return el("td", "perf-share", el("span", "perf-bar-track", bar), text ? el("span", "perf-share-figure", text) : null);
  }

  interface TrendSeries { values: (number | null)[]; ageS: number[]; newestUnixMs: number; limitUs: number }

  /**
   * The worst value of each ten-second window as a strip of bars across the
   * last half hour. Bars are clipped at three times the limit, so ordinary
   * variation stays visible beside a spike; a bar at or over the limit is
   * marked.
   */
  function trendStrip(series: TrendSeries, subject: string): HTMLElement {
    const namespace = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(namespace, "svg");
    svg.setAttribute("class", "perf-trend-chart");
    svg.setAttribute("viewBox", `0 0 ${TREND_SLOTS} 100`);
    svg.setAttribute("preserveAspectRatio", "none");
    svg.setAttribute("role", "img");
    const ceiling = series.limitUs * 3;
    const slots = new Map<number, number>();
    let over = 0;
    series.values.forEach((value, index) => {
      const slot = TREND_SLOTS - 1 - Math.round((series.ageS[index] ?? 0) / TREND_SLOT_S);
      if (value === null || slot < 0) return;
      slots.set(slot, value);
      if (value >= series.limitUs) over++;
      const height = Math.max(3, Math.min(1, value / ceiling) * 100);
      const bar = document.createElementNS(namespace, "rect");
      bar.setAttribute("class", value >= series.limitUs ? "perf-trend-bar perf-trend-over" : "perf-trend-bar");
      bar.setAttribute("x", String(slot + 0.15));
      bar.setAttribute("y", String(100 - height));
      bar.setAttribute("width", "0.7");
      bar.setAttribute("height", String(height));
      svg.append(bar);
    });
    const limit = document.createElementNS(namespace, "line");
    limit.setAttribute("class", "perf-trend-limit");
    limit.setAttribute("x1", "0");
    limit.setAttribute("x2", String(TREND_SLOTS));
    limit.setAttribute("y1", String(100 - 100 / 3));
    limit.setAttribute("y2", String(100 - 100 / 3));
    svg.append(limit);
    const summary = `Worst per 10 s, last 30 min. Line at ${formatLimit(series.limitUs)}.`;
    svg.setAttribute("aria-label", `Worst ${subject} per 10 seconds over the last 30 minutes. `
      + `${over} of ${slots.size} periods reached ${formatLimit(series.limitUs)}.`);
    const readout = el("figcaption", "perf-trend-caption", summary);
    svg.addEventListener("pointermove", (event) => {
      const box = svg.getBoundingClientRect();
      const slot = Math.max(0, Math.min(TREND_SLOTS - 1, Math.floor((event.clientX - box.left) / box.width * TREND_SLOTS)));
      const value = slots.get(slot);
      const at = formatClock(series.newestUnixMs - (TREND_SLOTS - 1 - slot) * TREND_SLOT_S * 1000);
      readout.textContent = value === undefined ? `${at}: nothing recorded` : `${at}: worst ${formatPerformanceDuration(value)}`;
    });
    svg.addEventListener("pointerleave", () => { readout.textContent = summary; });
    return el("figure", "perf-trend", svg as unknown as HTMLElement, readout);
  }

  function fact(name: string, value: string, over = false): HTMLElement {
    return el("div", "perf-fact", el("dt", "", name), el("dd", over ? "perf-over" : "", value));
  }
  function figure(title: string, headline: string, caption: string, facts: HTMLElement[], trend: HTMLElement | null): HTMLElement {
    const value = el("p", "perf-headline", el("span", "perf-value", headline), el("span", "perf-caption", caption));
    return el("section", "perf-figure", el("h3", "", title), value, facts.length ? el("dl", "perf-facts", ...facts) : null, trend);
  }

  function trackFigure(name: string, report: PerformanceReport): HTMLElement {
    const words = trackWords(name);
    const track = report.tracks.find((candidate) => candidate.track === name && candidate.completed > 0);
    const history = report.trend?.tracks[name];
    const limitUs = track?.budgetUs || history?.budgetUs || 0;
    const trend = report.trend && history && limitUs
      ? trendStrip({ values: history.maxUs, ageS: report.trend.ageS, newestUnixMs: report.trend.newestUnixMs, limitUs }, words.one)
      : null;
    if (!track) return figure(words.title, "–", `no ${words.many} in the last minute`, [], trend);
    const many = words.many.charAt(0).toUpperCase() + words.many.slice(1);
    return figure(
      words.title,
      formatPerformanceDuration(track.total.meanUs ?? 0),
      "mean time inside the host",
      [
        fact("95% within", formatPerformanceDuration(track.total.p95Us)),
        fact("Worst", formatPerformanceDuration(track.total.maxUs), track.budgetUs > 0 && track.total.maxUs >= track.budgetUs),
        fact(many, formatCount(track.completed)),
        track.budgetUs > 0
          ? fact(`Took ${formatLimit(track.budgetUs)} or more`, formatCount(track.slow), track.slow > 0)
          : null,
      ].filter((item): item is HTMLElement => item !== null),
      trend,
    );
  }

  interface HostCall { plugin: string; health: boolean; metric: PerformanceMetric }
  /** Plugin calls on the host loop, longest first. */
  function hostCalls(report: PerformanceReport): HostCall[] {
    return report.calls
      .map((metric) => ({ metric, ...callParts(metric.stage) }))
      .filter((call) => holdsHostLoop(call.kind) && call.plugin !== null)
      .map((call) => ({ plugin: call.plugin as string, health: call.kind === "host.health", metric: call.metric }))
      .sort((left, right) => right.metric.maxUs - left.metric.maxUs);
  }

  function hostFigure(report: PerformanceReport): HTMLElement {
    const pass = report.hostPass;
    const trend = report.trend
      ? trendStrip({
        values: report.trend.hostPassMaxUs, ageS: report.trend.ageS, newestUnixMs: report.trend.newestUnixMs, limitUs: LONG_CALL_US,
      }, "host loop pass")
      : null;
    if (!pass || pass.calls === 0) return figure("Host loop", "–", "no passes recorded", [], trend);
    const longest = hostCalls(report)[0];
    return figure(
      "Host loop",
      formatPerformanceDuration(pass.meanUs ?? 0),
      "mean pass over every plugin",
      [
        fact("95% within", formatPerformanceDuration(pass.p95Us)),
        fact("Worst pass", formatPerformanceDuration(pass.maxUs), pass.maxUs >= LONG_CALL_US),
        fact("Passes per second", formatCount(pass.calls / Math.max(1, report.periodUs / 1_000_000))),
        longest
          ? fact("Longest call", `${performancePluginName(longest.plugin)}, ${formatPerformanceDuration(longest.metric.maxUs)}`,
            longest.metric.maxUs >= LONG_CALL_US)
          : null,
      ].filter((item): item is HTMLElement => item !== null),
      trend,
    );
  }

  function stageTable(track: PerformanceTrack): HTMLElement {
    const total = track.steps.reduce((sum, step) => sum + step.totalUs, 0);
    const rows = track.steps.map((step) => {
      const name = el("td", "", el("span", "perf-name", stageName(step.stage)), el("small", "perf-id", step.stage.replace(">", " → ")));
      const share = total > 0 ? step.totalUs / total : 0;
      return el("tr", "", name, proportion(share, `${Math.round(share * 100)}%`),
        durationCell(step.meanUs), durationCell(step.p95Us), durationCell(step.maxUs, track.budgetUs));
    });
    return el("div", "perf-block",
      el("h4", "", trackWords(track.track).title),
      dataTable([["Stage", false], ["Share of time", false], ["Mean", true], ["95% within", true], ["Worst", true]], rows));
  }

  function callTable(report: PerformanceReport): HTMLElement {
    const calls = hostCalls(report).slice(0, LISTED_CALLS);
    if (!calls.length) return el("p", "muted", "No plugin call took 0.25 ms or longer in the last minute.");
    const longest = Math.max(...calls.map((call) => call.metric.maxUs), 1);
    const rows = calls.map((call) => {
      const title = performancePluginName(call.plugin) + (call.health ? " (health check)" : "");
      const name = el("td", "", el("span", "perf-name", title), el("small", "perf-id", call.plugin));
      return el("tr", "", name, proportion(call.metric.maxUs / longest, ""),
        durationCell(call.metric.maxUs, LONG_CALL_US), durationCell(call.metric.meanUs), countCell(call.metric.calls));
    });
    return dataTable([["Plugin", false], ["", false], ["Longest call", true], ["Mean", true], ["Calls", true]], rows);
  }

  function momentTable(report: PerformanceReport): HTMLElement {
    const moments = report.moments.slice(0, LISTED_MOMENTS);
    if (!moments.length) return el("p", "muted", "Nothing exceeded its limit in the retained history.");
    const rows = moments.map((moment) => {
      const time = el("td", "perf-time");
      // A run that lasted is shown from its first member to its last.
      time.textContent = moment.untilUnixMs - moment.observedUnixMs >= 1_000
        ? `${formatClock(moment.observedUnixMs)} to ${formatClock(moment.untilUnixMs)}`
        : formatClock(moment.observedUnixMs);
      return el("tr", "", time, durationCell(moment.us), el("td", "", performanceMomentText(moment)));
    });
    return dataTable([["Time", false], ["Longest", true], ["What happened", false]], rows);
  }

  /** The panel body for one report. */
  export function performanceContent(report: PerformanceReport): HTMLElement {
    if (report.periodUs === 0) {
      return el("p", "muted", report.collecting
        ? "Collecting the first ten seconds of measurements."
        : "Nothing has been measured yet. Measurement begins when the first plugin starts.");
    }
    const others = report.tracks.map((track) => track.track).filter((name) => name !== "input" && name !== "video");
    const staged = report.tracks.filter((track) => track.completed > 0 && track.steps.length > 0);
    return el("div", "perf",
      el("div", "perf-summary", ...["input", "video", ...others].map((name) => trackFigure(name, report)), hostFigure(report)),
      staged.length
        ? el("section", "perf-section",
          el("h3", "", "Where the time goes"),
          el("p", "muted", "Each step from arrival to hand-off, measured inside the host. Network, client and display time are not included."),
          ...staged.map(stageTable))
        : null,
      el("section", "perf-section",
        el("h3", "", "Plugin calls on the host loop"),
        el("p", "muted", "Every plugin takes its turn on one loop, so a long call delays input and video for all of them. Calls under 0.25 ms are not listed."),
        callTable(report)),
      el("section", "perf-section",
        el("h3", "", "Slow moments"),
        el("p", "muted", `Input and video that took longer than their limit, and plugin calls of ${formatLimit(LONG_CALL_US)} or more. Newest first.`),
        momentTable(report)),
      report.droppedRecords
        ? el("p", "muted", `${formatCount(report.droppedRecords)} measurements were dropped in the last minute because the recorder was full.`)
        : null,
    );
  }

  /** Renders the performance panel and keeps it current while it is visible. */
  export function renderPerformance(context: PageContext, target: HTMLElement): () => Promise<void> {
    const REFRESH_MS = 10_000;
    const body = el("div", "perf-body", el("p", "muted", "Loading measurements."));
    const status = el("p", "muted perf-status");
    status.setAttribute("role", "status");
    target.replaceChildren(panel(
      "Performance",
      el("p", "muted", "How long input and video spend inside the host over the last minute, and which plugin holds things up."),
      body,
      status,
    ));
    let generation = 0;
    let shown = false;
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
      try {
        const report = parsePerformanceReply(await context.api.post<unknown>("diagnostics/performance"));
        if (current !== generation || !target.isConnected) return;
        shown = true;
        if (!report) {
          body.replaceChildren(el("p", "muted", "This host does not measure performance yet. A host update adds it."));
          status.textContent = "";
          return;
        }
        body.replaceChildren(performanceContent(report));
        status.textContent = `Updated ${formatClock(Date.now())}. Refreshes every 10 seconds.`;
      } catch (error) {
        if (current !== generation || !target.isConnected) return;
        // Figures already on screen stay there; a failed refresh only says so.
        if (shown) status.textContent = `Could not refresh at ${formatClock(Date.now())}: ${messageOf(error)}. Trying again shortly.`;
        else body.replaceChildren(errorPanel(error, load));
      } finally {
        if (current === generation) schedule();
      }
    }
    void load();
    return load;
  }
}
