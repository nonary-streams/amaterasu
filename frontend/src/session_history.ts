namespace Amaterasu {
  type RecordedDetail = NonNullable<HistoryDetailReply["detail"]>;
  type RecordedSample = RecordedDetail["samples"][number];
  type SampleValue = (sample: RecordedSample) => number | null;
  type ChartSource = "recorded" | "live";
  type ChartSample = RecordedSample & { chart_source?: ChartSource; interval_start_unix_ms?: number };

  /** Adjacent records with matching names may be reconnects; they are not proven identities. */
  export function groupRecordedSessions(sessions: HistorySessionSummary[]): HistorySessionSummary[][] {
    const groups: HistorySessionSummary[][] = [];
    for (const session of sessions) {
      const group = groups.at(-1);
      const newer = group?.at(-1);
      const identityKnown = !!session.protocol.trim() && !!(session.client_name.trim() || session.device_name.trim());
      const matching = newer && identityKnown && newer.protocol === session.protocol &&
        newer.app_name === session.app_name && newer.client_name === session.client_name &&
        newer.device_name === session.device_name;
      const gap = newer && (newer.ended_at_unix === null || newer.ended_at_unix >= newer.started_at_unix) &&
        session.ended_at_unix !== null && session.ended_at_unix >= session.started_at_unix
        ? newer.started_at_unix - session.ended_at_unix : null;
      if (group && matching && gap !== null && gap >= 0 && gap <= 120) group.push(session);
      else groups.push([session]);
    }
    return groups;
  }

  /** Combine bounded member snapshots without erasing their individual identities. */
  export function mergeRecordedSessionDetails(members: RecordedDetail[]): RecordedDetail {
    if (members.length < 2) throw new Error("A grouped history needs at least two streams.");
    const ordered = [...members].sort((a, b) => a.session.started_at_unix - b.session.started_at_unix);
    const first = ordered[0]!;
    const last = ordered.at(-1)!;
    const total = (read: (detail: RecordedDetail) => number): number => {
      const result = ordered.reduce((sum, member) => sum + read(member), 0);
      if (!Number.isSafeInteger(result)) throw new Error("Grouped history counts exceed the browser limit.");
      return result;
    };
    const same = <K extends "width" | "height" | "target_fps" | "target_bitrate_kbps" | "codec">(key: K) =>
      ordered.every((member) => member.session[key] === last.session[key]);
    const duration = ordered.every((member) => member.session.duration_seconds !== null)
      ? total((member) => member.session.duration_seconds!) : null;
    return {
      session: { ...last.session, started_at_unix:first.session.started_at_unix, duration_seconds:duration,
        width:same("width") ? last.session.width : 0, height:same("height") ? last.session.height : 0,
        target_fps:same("target_fps") ? last.session.target_fps : 0,
        target_bitrate_kbps:same("target_bitrate_kbps") ? last.session.target_bitrate_kbps : 0,
        codec:same("codec") ? last.session.codec : "" },
      sample_count:total((member) => member.sample_count), event_count:total((member) => member.event_count),
      samples_truncated:ordered.some((member) => member.samples_truncated),
      events_truncated:ordered.some((member) => member.events_truncated),
      samples:ordered.flatMap((member) => member.samples), events:ordered.flatMap((member) => member.events),
    };
  }

  function historyLiteral(value: string): TextValue {
    return typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(value) : value;
  }

  function historyTime(unixSeconds: number): Node | string {
    const date = new Date(unixSeconds * 1000);
    return typeof Amaterasu.localeFormattedTextNode === "function"
      ? Amaterasu.localeFormattedTextNode((locale) => date.toLocaleString(locale))
      : date.toLocaleString();
  }

  function historyValue(value: number | null | undefined, unit: string, digits = 1): string {
    return value == null ? "Unavailable" : `${value.toFixed(digits)} ${unit}`;
  }

  function historySummaryItem(label: string, value: Node | TextValue): HTMLElement {
    return el("div", "recorded-session-summary-item", el("dt", "", label), el("dd", "", value));
  }

  function recordedChart(
    label: string, unit: string, samples: RecordedSample[], valueOf: SampleValue, source: "recorded" | "live" | "combined" = "recorded",
  ): HTMLElement {
    const values = samples.map(valueOf);
    const available = values.filter((value): value is number => value !== null);
    const current = values.at(-1);
    const maximum = Math.max(1, ...available);
    const orderedTime = samples.every((sample, index) => index === 0 ||
      sample.recorded_at_unix > samples[index - 1]!.recorded_at_unix);
    const start = samples[0]?.recorded_at_unix ?? 0;
    const span = (samples.at(-1)?.recorded_at_unix ?? start) - start;
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 320 84");
    svg.setAttribute("width", "320");
    svg.setAttribute("height", "84");
    svg.setAttribute("role", "img");
    const observation = source === "live" ? "observed " : source === "recorded" ? "recorded " : "";
    svg.setAttribute("aria-label", `${label}; ${available.length} ${observation}${available.length === 1 ? "value" : "values"}, latest ${historyValue(current, unit, unit === "gaps" ? 0 : 1)}.`);
    svg.setAttribute("class", "host-metric-chart");
    const axis = document.createElementNS("http://www.w3.org/2000/svg", "path");
    axis.setAttribute("d", "M 0 78 H 320");
    axis.setAttribute("class", "host-metric-chart-axis");
    axis.setAttribute("stroke", "currentColor");
    axis.setAttribute("stroke-opacity", "0.3");
    svg.append(axis);
    const segments: { source: ChartSource; points: { x: number; y: number }[] }[] = [];
    let segment: { x: number; y: number }[] = [];
    let segmentSource: ChartSource = source === "live" ? "live" : "recorded";
    values.forEach((value, index) => {
      const pointSource = (samples[index] as ChartSample).chart_source ?? (source === "live" ? "live" : "recorded");
      if (index > 0 && (samples[index]!.session_id !== samples[index - 1]!.session_id || pointSource !== segmentSource)) {
        if (segment.length) segments.push({ source:segmentSource, points:segment });
        segment = [];
      }
      if (value === null) {
        if (segment.length) segments.push({ source:segmentSource, points:segment });
        segment = [];
        return;
      }
      segmentSource = pointSource;
      const timestamp = samples[index]!.recorded_at_unix;
      const x = orderedTime && span > 0
        ? (timestamp - start) / span * 320
        : index / Math.max(samples.length - 1, 1) * 320;
      segment.push({ x, y: 78 - value / maximum * 72 });
    });
    if (segment.length) segments.push({ source:segmentSource, points:segment });
    for (const { source:segmentKind, points } of segments) {
      if (points.length === 1) {
        const point = points[0]!;
        const dot = document.createElementNS("http://www.w3.org/2000/svg", "circle");
        dot.setAttribute("cx", String(point.x));
        dot.setAttribute("cy", String(point.y));
        dot.setAttribute("r", "3");
        dot.setAttribute("class", `host-metric-chart-line${segmentKind === "live" ? " host-metric-chart-live" : ""}`);
        svg.append(dot);
      } else {
        const line = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
        line.setAttribute("points", points.map((point) => `${point.x},${point.y}`).join(" "));
        line.setAttribute("class", `host-metric-chart-line${segmentKind === "live" ? " host-metric-chart-live" : ""}`);
        if (segmentKind === "live") line.setAttribute("stroke-dasharray", "4 3");
        line.setAttribute("fill", "none");
        line.setAttribute("stroke", "currentColor");
        line.setAttribute("stroke-width", "2");
        svg.append(line);
      }
    }
    return el("div", "host-metric-chart-card",
      el("div", "host-metric-chart-heading", el("strong", "", label),
        el("span", "", historyLiteral(historyValue(current, unit, unit === "gaps" ? 0 : 1)))),
      svg,
      el("small", "muted", historyLiteral(available.length
        ? `${available.length} ${observation}values · scale 0–${maximum.toFixed(unit === "gaps" ? 0 : 1)} ${unit}`
        : source === "live" ? "No observed values." : "No recorded values.")));
  }

  export interface CombinedActiveSamples {
    samples: ChartSample[];
    recordedCount: number;
    liveCount: number;
    note: string;
  }

  /** Append only live intervals that begin after the last retained one-second timestamp. */
  export function combineActiveSamples(
    recorded: RecordedDetail | null, live: RecordedDetail, retentionSeconds: number, maxPoints: number,
  ): CombinedActiveSamples {
    const livePoints = (live.samples as ChartSample[]).filter((sample) => sample.chart_source === "live" &&
      Number.isSafeInteger(sample.interval_start_unix_ms) && sample.session_id === live.session.session_id);
    const recordedPoints = (recorded?.samples ?? []).map((sample): ChartSample => ({ ...sample, chart_source:"recorded" }));
    let note = "";
    let appended = livePoints;
    if (recordedPoints.length) {
      const ordered = recordedPoints.every((sample, index) => sample.session_id === live.session.session_id &&
        (index === 0 || sample.recorded_at_unix > recordedPoints[index - 1]!.recorded_at_unix));
      const last = recordedPoints.at(-1)!.recorded_at_unix;
      const boundary = (last + 1) * 1000;
      if (!ordered || !Number.isSafeInteger(boundary) || (livePoints.length > 0 &&
          livePoints.at(-1)!.recorded_at_unix * 1000 < last * 1000)) {
        appended = [];
        note = "Recorded and live times cannot be safely joined; select Live only for current observations.";
      } else {
        appended = livePoints.filter((sample) => sample.interval_start_unix_ms! >= boundary);
        if (livePoints.length && !appended.length) note = "Recent live intervals overlap recorded samples; select Live only to inspect them.";
      }
    }
    const combined = [...recordedPoints, ...appended];
    const newest = combined.length ? Math.max(...combined.map((sample) => sample.recorded_at_unix)) : 0;
    const samples = combined.filter((sample) => sample.recorded_at_unix >= newest - retentionSeconds).slice(-maxPoints);
    return { samples, recordedCount:samples.filter((sample) => sample.chart_source === "recorded").length,
      liveCount:samples.filter((sample) => sample.chart_source === "live").length, note };
  }

  export interface LiveSessionCard {
    element: HTMLElement;
    update(session: HistorySessionSummary, live: RecordedDetail, recorded: RecordedDetail | null,
      historyNote: string, rounded: boolean, retentionSeconds: number, maxPoints: number): void;
  }

  /** Keep the selected source and range mounted while observations arrive. */
  export function createLiveSessionCard(): LiveSessionCard {
    const heading = el("div");
    const liveSummary = el("p", "live-session-current");
    const sourceControls = el("div", "recorded-session-ranges live-session-sources");
    sourceControls.setAttribute("role", "group");
    sourceControls.setAttribute("aria-label", "Stream chart source");
    const controls = el("div", "recorded-session-ranges");
    controls.setAttribute("role", "group");
    controls.setAttribute("aria-label", "Live session sample range");
    const status = el("p", "muted recorded-session-range-status");
    const legend = el("p", "muted live-session-legend");
    const charts = el("div");
    const exportButton = button("Download chart data (CSV)", () => downloadChartCsv(), "quiet");
    const exportStatus = el("p", "muted");
    exportStatus.setAttribute("role", "status");
    const choices: { minutes: number | null; node: HTMLButtonElement }[] = [];
    type Mode = "combined" | "live" | "recorded";
    const sourceChoices: { mode: Mode; node: HTMLButtonElement }[] = [];
    let mode: Mode = "combined";
    let range: number | null = null;
    let live: RecordedDetail | null = null;
    let recorded: RecordedDetail | null = null;
    let historyNote = "";
    let rounded = false;
    let retentionSeconds = 300;
    let maxPoints = 300;
    let currentPoints: ChartSample[] = [];
    let currentSession: HistorySessionSummary | null = null;

    function draw(): void {
      for (const choice of sourceChoices) choice.node.setAttribute("aria-pressed", String(choice.mode === mode));
      for (const choice of choices) choice.node.setAttribute("aria-pressed", String(choice.minutes === range));
      if (!live) {
        currentPoints = [];
        exportButton.disabled = true;
        charts.replaceChildren();
        return;
      }
      const latestLive = live.samples.at(-1);
      liveSummary.textContent = latestLive
        ? `Latest live rate: ${historyValue(latestLive.actual_fps, "fps")} · ${historyValue(latestLive.actual_bitrate_kbps / 1000, "Mbit/s")} · host observation ${new Date(latestLive.recorded_at_unix * 1000).toLocaleString()}.${rounded ? " Very large counters are rounded for display." : ""}`
        : "Latest live rate: waiting for two host counter readings.";
      if (mode === "recorded" && !recorded) {
        currentPoints = [];
        exportButton.disabled = true;
        status.textContent = historyNote || "Recorded history is unavailable. Select Live only to see current observations.";
        legend.textContent = "";
        charts.replaceChildren();
        return;
      }
      const combined = combineActiveSamples(recorded, live, retentionSeconds, maxPoints);
      const points: ChartSample[] = mode === "live" ? live.samples as ChartSample[] : mode === "recorded"
        ? (recorded?.samples ?? []).map((sample): ChartSample => ({...sample, chart_source:"recorded"})) : combined.samples;
      const newest = points.length ? Math.max(...points.map((sample) => sample.recorded_at_unix)) : 0;
      const selectedRange = range;
      const selected = selectedRange === null ? points : points.filter((sample) => sample.recorded_at_unix >= newest - selectedRange * 60);
      currentPoints = selected;
      exportButton.disabled = selected.length === 0;
      const recordedCount = points.filter((sample) => sample.chart_source === "recorded").length;
      const liveCount = points.filter((sample) => sample.chart_source === "live").length;
      const description = mode === "combined" ? `${recordedCount} recorded · ${liveCount} live.`
        : mode === "live" ? "Live observations stay in this browser." : "Recorded samples are retained by the host.";
      status.textContent = `${selected.length} of ${points.length} samples shown. ${description}${mode === "combined" && combined.note ? ` ${combined.note}` : ""}${historyNote ? ` ${historyNote}` : ""}`;
      legend.textContent = mode === "combined" && liveCount ? "Recorded: solid / filled · Live: dashed / hollow" : "";
      if (!selected.length) {
        charts.replaceChildren(empty("No samples in this range", points.length ? "Choose a longer range." : mode === "recorded" ? "Waiting for recorded samples." : "Waiting for another live counter reading."));
        return;
      }
      const chartSource = mode === "combined" ? "combined" : mode;
      charts.replaceChildren(el("div", "recorded-session-chart-grid",
        recordedChart("Accepted video rate", "fps", selected, (sample) => sample.actual_fps, chartSource),
        recordedChart("Accepted audio/video payload rate", "Mbit/s", selected, (sample) => sample.actual_bitrate_kbps / 1000, chartSource),
        recordedChart("Encoder latency", "ms", selected, (sample) => sample.encode_latency_ms ?? null, chartSource),
        recordedChart("Observed video sequence gaps", "gaps", selected, (sample) => sample.dropped_frames, chartSource)));
    }

    function downloadChartCsv(): void {
      if (!currentSession || !currentPoints.length) return;
      const columns = ["session_id", "recorded_at_unix", "observed_at", "source", "frames_sent", "bytes_sent",
        "dropped_frames", "actual_fps", "actual_bitrate_kbps", "encode_latency_ms"] as const;
      const rows = currentPoints.map((sample) => {
        const observedAt = new Date(sample.recorded_at_unix * 1000);
        return [
          sample.session_id,
          sample.recorded_at_unix,
          Number.isFinite(observedAt.getTime()) ? observedAt.toISOString() : "",
          sample.chart_source ?? (mode === "live" ? "live" : "recorded"),
          sample.frames_sent,
          sample.bytes_sent,
          sample.dropped_frames,
          sample.actual_fps,
          sample.actual_bitrate_kbps,
          sample.encode_latency_ms,
        ];
      });
      const formulaLeadingText = /^[\s\u0000-\u001f\u007f-\u009f]*[=+\-@]/u;
      const unsafeLeadingControl = /^[\t\r\n]/;
      const csvCell = (value: unknown): string => {
        const text = String(value ?? "");
        const spreadsheetSafeText = typeof value === "string" &&
          (formulaLeadingText.test(text) || unsafeLeadingControl.test(text)) ? `'${text}` : text;
        return `"${spreadsheetSafeText.replaceAll(String.fromCharCode(34), String.fromCharCode(34, 34))}"`;
      };
      const csv = [columns, ...rows].map((row) => row.map(csvCell).join(",")).join("\r\n") + "\r\n";
      const objectUrl = URL.createObjectURL(new Blob([csv], { type: "text/csv;charset=utf-8" }));
      const link = document.createElement("a");
      const safeId = currentSession.session_id.replace(/[^a-zA-Z0-9_-]/g, "_").slice(0, 80) || "stream";
      link.href = objectUrl;
      link.download = `amaterasu-stream-${safeId}.csv`;
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(objectUrl), 0);
      exportStatus.textContent = `Downloaded ${currentPoints.length} chart samples as CSV.`;
    }
    for (const [nextMode, label] of [["combined", "History + live"], ["live", "Live only"], ["recorded", "Recorded only"]] as const) {
      const node = button(label, () => { mode = nextMode; draw(); }, "recorded-range-button");
      sourceChoices.push({mode:nextMode, node});
      sourceControls.append(node);
    }
    for (const minutes of [null, 3, 5, 15, 30, 60] as const) {
      const node = button(minutes === null ? "Available samples" : `${minutes} min`, () => { range = minutes; draw(); }, "recorded-range-button");
      choices.push({ minutes, node });
      controls.append(node);
    }
    const element = el("article", "live-session-card", heading,
      el("p", "muted", "Host-side accepted media and observed sequence gaps; client delivery is not confirmed."),
      liveSummary, sourceControls, controls, status, legend, charts, exportButton, exportStatus);
    return { element, update(session, nextLive, nextRecorded, nextNote, nextRounded, nextRetention, nextMaximum) {
      currentSession = session;
      live = nextLive;
      recorded = nextRecorded;
      historyNote = nextNote;
      rounded = nextRounded;
      retentionSeconds = nextRetention;
      maxPoints = nextMaximum;
      const resolution = session.width && session.height ? `${session.width} × ${session.height}` : "Resolution unavailable";
      const fps = session.target_fps ? ` · ${session.target_fps} fps target` : "";
      const title = session.app_name || session.device_name || "Live stream";
      const client = session.app_name ? session.device_name || session.client_name || "Client unavailable"
        : session.device_name ? "" : `Stream ${session.session_id}`;
      heading.replaceChildren(el("div", "live-session-heading",
        el("div", "", el("span", "live-session-label", "Active"),
          el("h3", "", historyLiteral(title)),
          el("p", "muted", historyLiteral([client, session.protocol || "Protocol unavailable"].filter(Boolean).join(" · ")))),
        el("p", "muted", historyLiteral(`${resolution}${fps} · ${session.codec || "Codec unavailable"}`))));
      draw();
    } };
  }

  export function renderRecordedSessionDetail(detail: RecordedDetail, close: () => void, complete = false, members?: RecordedDetail[]): HTMLElement {
    const session = detail.session;
    const chartArea = el("div");
    const rangeStatus = el("p", "muted recorded-session-range-status");
    rangeStatus.setAttribute("role", "status");
    const rangeControls = el("div", "recorded-session-ranges");
    rangeControls.setAttribute("role", "group");
    rangeControls.setAttribute("aria-label", "Recorded sample range");
    const rangeButtons: { minutes: number | null; node: HTMLButtonElement }[] = [];
    const newest = detail.samples.length ? Math.max(...detail.samples.map((sample) => sample.recorded_at_unix)) : 0;
    let range: number | null = null;
    function draw(): void {
      const selectedRange = range;
      const samples = selectedRange === null ? detail.samples : detail.samples.filter((sample) => sample.recorded_at_unix >= newest - selectedRange * 60);
      for (const choice of rangeButtons) choice.node.setAttribute("aria-pressed", String(choice.minutes === range));
      if (!samples.length) {
        chartArea.replaceChildren(empty("No samples in this range", detail.samples.length
          ? "Choose a longer range to inspect retained samples." : "No samples were recorded for this session."));
        rangeStatus.textContent = "No recorded samples in this range.";
        return;
      }
      const ordered = samples.every((sample, index) => index === 0 || sample.recorded_at_unix > samples[index - 1]!.recorded_at_unix);
      const spacing = samples.length === 1 ? " · single sample" : ordered
        ? " · time-proportional spacing" : " · evenly spaced in recorded order because timestamps repeat or move backward";
      rangeStatus.textContent = `${samples.length} of ${detail.samples.length} returned samples${spacing}.`;
      chartArea.replaceChildren(el("div", "recorded-session-chart-grid",
        recordedChart("Accepted video rate", "fps", samples, (sample) => sample.actual_fps),
        recordedChart("Accepted audio/video payload rate", "Mbit/s", samples, (sample) => sample.actual_bitrate_kbps / 1000),
        recordedChart("Encoder latency", "ms", samples, (sample) => sample.encode_latency_ms ?? null),
        recordedChart("Observed video sequence gaps", "gaps", samples, (sample) => sample.dropped_frames)));
    }
    for (const minutes of [null, 3, 5, 15, 30, 60] as const) {
      const node = button(minutes === null ? "Available samples" : `${minutes} min`, () => { range = minutes; draw(); }, "recorded-range-button");
      rangeButtons.push({ minutes, node });
      rangeControls.append(node);
    }
    const download = button(complete ? "Download retained history (JSON)" : "Download returned history (JSON)", () => {
      const payload = JSON.stringify(members
        ? { format: "amaterasu-session-history-group/1", scope: complete ? "retained_snapshots" : "recent_snapshots", members }
        : { format: "amaterasu-session-history/1", scope: complete ? "retained_snapshot" : "recent_snapshot", detail }, null, 2);
      const url = URL.createObjectURL(new Blob([payload], { type: "application/json;charset=utf-8" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = members ? "amaterasu-session-history-group.json" : "amaterasu-session-history.json";
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 0);
    });
    const resolution = session.width && session.height ? `${session.width} × ${session.height}` : "Unavailable";
    const targetFps = session.target_fps ? `${session.target_fps} fps` : "Unavailable";
    const targetBitrate = session.target_bitrate_kbps ? `${(session.target_bitrate_kbps / 1000).toFixed(1)} Mbit/s` : "Unavailable";
    const duration = session.duration_seconds === null ? (session.ended_at_unix === null ? "Active" : "Unavailable") : `${session.duration_seconds} seconds`;
    const sampleArea = el("div");
    let samplePage = 0;
    function drawSamples(): void {
      const start = samplePage * 50;
      const visible = detail.samples.slice(start, start + 50);
      sampleArea.replaceChildren(visible.length ? table(
        [...(members ? ["Stream"] : []), "Recorded time", "Accepted frames", "Accepted bytes", "Sequence gaps", "Video fps", "Payload kbps", "Encoder latency"],
        visible.map((sample) => [...(members ? [historyLiteral(sample.session_id)] : []), historyTime(sample.recorded_at_unix), String(sample.frames_sent), String(sample.bytes_sent),
          String(sample.dropped_frames), sample.actual_fps.toFixed(1), sample.actual_bitrate_kbps.toFixed(1), historyValue(sample.encode_latency_ms, "ms")]),
      ) : el("p", "muted", "No samples recorded."),
      ...(detail.samples.length > 50 ? [pager(samplePage > 0 ? () => { samplePage--; drawSamples(); } : null,
        start + visible.length < detail.samples.length ? () => { samplePage++; drawSamples(); } : null,
        historyLiteral(`Samples ${start + 1}–${start + visible.length} of ${detail.samples.length}`))] : []));
    }
    drawSamples();
    const sampleDisclosure = el("details", "recorded-session-disclosure", el("summary", "", "Sample table"), sampleArea);
    const eventDisclosure = el("details", "recorded-session-disclosure", el("summary", "", "Recorded events"));
    const eventArea = el("div");
    let eventPage = 0;
    function drawEvents(): void {
      const start = eventPage * 25;
      const visible = detail.events.slice(start, start + 25);
      const rows = visible.map((event) => {
        const row = el("div", "recorded-session-event", el("strong", "", historyLiteral(event.event_type)),
          el("small", "muted", historyTime(event.recorded_at_unix)),
          ...(members ? [el("small", "muted", historyLiteral(`Stream ${event.session_id}`))] : []));
        if (event.payload != null) row.append(el("details", "", el("summary", "", "Event details"),
          el("pre", "recorded-session-event-payload", historyLiteral(JSON.stringify(event.payload, null, 2)))));
        return row;
      });
      eventArea.replaceChildren(...(rows.length ? rows : [el("p", "muted", "No events recorded.")]),
        ...(detail.events.length > 25 ? [pager(eventPage > 0 ? () => { eventPage--; drawEvents(); } : null,
          start + visible.length < detail.events.length ? () => { eventPage++; drawEvents(); } : null,
          historyLiteral(`Events ${start + 1}–${start + visible.length} of ${detail.events.length}`))] : []));
    }
    drawEvents();
    eventDisclosure.append(eventArea);
    const info = el("dl", "recorded-session-summary",
      historySummaryItem("Client", historyLiteral(session.client_name || session.device_name || "Unavailable")),
      historySummaryItem("Protocol", historyLiteral(session.protocol || "Unavailable")),
      historySummaryItem("Resolution", historyLiteral(resolution)),
      historySummaryItem("Target frame rate", historyLiteral(targetFps)),
      historySummaryItem("Target bitrate", historyLiteral(targetBitrate)),
      historySummaryItem("Codec", historyLiteral(session.codec || "Unavailable")),
      historySummaryItem(members ? "Total stream time" : "Duration", historyLiteral(duration)),
      historySummaryItem("Started", historyTime(session.started_at_unix)),
    );
    const heading = el("div", "recorded-session-heading",
      el("div", "", el("h2", "", historyLiteral(`History for ${session.app_name || (members ? "Unnamed application" : session.session_id)}${members ? ` · ${members.length} nearby streams` : ""}`)),
        el("p", "muted", historyLiteral(`${detail.samples.length} of ${detail.sample_count} samples · ${detail.events.length} of ${detail.event_count} events`))),
      el("div", "actions", download, button("Close details", close)));
    const limitation = detail.samples_truncated || detail.events_truncated
      ? el("p", "notice", "Only the latest returned samples and events are available here and in the JSON download.")
      : el("p", "muted", complete
        ? members ? "The JSON download contains each retained snapshot regardless of chart range. Older records may have been removed before the snapshots."
          : "The JSON download contains the retained snapshot regardless of chart range. Older records may have been removed before this snapshot."
        : "The JSON download contains all returned session metadata, samples and events regardless of chart range.");
    const groupNote = members ? el("p", "muted", "These streams are grouped by matching names and a short time gap; a reconnect is not confirmed. Chart lines stop between streams, and the download preserves each stream separately.") : null;
    const box = el("section", "panel recorded-session-detail", heading, info, groupNote, limitation,
      el("h3", "", "Recorded stream performance"),
      el("p", "muted", "Rates and counters describe host-side media accepted by the local transport queue, including audio payload bytes. Sequence gaps are not client-reported loss; client delivery is not confirmed. Zero rates can also represent a zero-duration final sample."),
      rangeControls, rangeStatus, chartArea, sampleDisclosure, eventDisclosure);
    draw();
    return box;
  }
}
