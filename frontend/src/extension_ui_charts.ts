namespace Amaterasu {
  export interface ExtensionChartPoint { time: number; value: number | null }
  export interface ExtensionChartSeries { id: string; label: string; points: readonly ExtensionChartPoint[] }
  export interface ExtensionUiMetricChartOptions {
    label: string;
    unit: string;
    series: readonly ExtensionChartSeries[];
    minimum?: number;
    maximum?: number;
    appearance?: "full" | "sparkline";
    xAxis?: "time" | "sample";
  }

  export interface ExtensionUiChartsV1 {
    metricGauge(options: { label: string; value: number | null; maximum?: number; unit?: string; detail?: string }): {
      element: HTMLElement; setValue(value: number | null, detail?: string): void; dispose(): void;
    };
    metricChart(options: ExtensionUiMetricChartOptions): {
      element: HTMLElement; setData(series: readonly ExtensionChartSeries[]): void; dispose(): void;
    };
  }

  export function validateExtensionChartSeries(series: readonly ExtensionChartSeries[], xAxis?: "time" | "sample"): void {
    if (!Array.isArray(series) || series.length > 8 || new Set(series.map((s) => s.id)).size !== series.length)
      throw new Error("Invalid chart series.");
    for (const item of series) {
      if (!item || typeof item.id !== "string" || !item.id || typeof item.label !== "string" || !item.label ||
          !Array.isArray(item.points) || item.points.length > 2000)
        throw new Error("Invalid chart series.");
      let previous = -Infinity;
      for (const point of item.points) {
        if (!point || !Number.isFinite(point.time) || point.time < previous ||
            (xAxis === "sample" && (!Number.isSafeInteger(point.time) || point.time < 1)) ||
            (point.value !== null && !Number.isFinite(point.value))) throw new Error("Invalid chart point.");
        previous = point.time;
      }
    }
  }

  function chartSvg<K extends keyof SVGElementTagNameMap>(tag: K, attributes: Record<string, string>): SVGElementTagNameMap[K] {
    const node = document.createElementNS("http://www.w3.org/2000/svg", tag);
    for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, value);
    return node;
  }

  export function createExtensionUiChartsV1(signal: AbortSignal): ExtensionUiChartsV1 {
    const ui: ExtensionUiChartsV1 = {
      metricGauge(options) {
        const maximum = options.maximum ?? 100;
        if (!options.label || !Number.isFinite(maximum) || maximum <= 0) throw new Error("Invalid metric gauge.");
        const dial = chartSvg("svg", { viewBox: "0 0 120 84", role: "img", class: "plugin-gauge-dial" });
        const track = chartSvg("path", { d: "M 15 65 A 45 45 0 1 1 105 65", fill: "none", stroke: "var(--line)", "stroke-width": "9", "stroke-linecap": "round" });
        const level = chartSvg("path", { d: "M 15 65 A 45 45 0 1 1 105 65", fill: "none", stroke: "var(--accent)", "stroke-width": "9", "stroke-linecap": "round", pathLength: "100" });
        const number = chartSvg("text", { x: "60", y: "64", "text-anchor": "middle", fill: "currentColor", "font-size": "20" });
        dial.append(track, level, number);
        const detail = el("small", "muted");
        const element = el("div", "plugin-metric-gauge", el("strong", "", options.label), dial, detail);
        let disposed = signal.aborted;
        const setValue = (value: number | null, description = "") => {
          if (value !== null && !Number.isFinite(value)) throw new Error("Invalid gauge value.");
          if (disposed) return;
          number.textContent = value === null ? "—" : `${value.toFixed(1)}${options.unit ?? "%"}`;
          level.setAttribute("stroke-dasharray", `${value === null ? 0 : Math.max(0, Math.min(100, value * 100 / maximum))} 100`);
          detail.textContent = description;
          dial.setAttribute("aria-label", `${options.label}: ${value === null ? "Unavailable" : number.textContent}. ${description}`);
        };
        const dispose = () => { disposed = true; signal.removeEventListener("abort", dispose); };
        signal.addEventListener("abort", dispose, { once: true });
        setValue(options.value, options.detail);
        return { element, setValue, dispose };
      },
      metricChart(options) {
        validateExtensionChartSeries(options.series, options.xAxis);
        if (!options.label || typeof options.unit !== "string" ||
            (options.minimum !== undefined && !Number.isFinite(options.minimum)) ||
            (options.maximum !== undefined && !Number.isFinite(options.maximum)) ||
            (options.minimum !== undefined && options.maximum !== undefined && options.minimum >= options.maximum))
          throw new Error("Invalid chart scale.");
        if (options.appearance !== undefined && options.appearance !== "full" && options.appearance !== "sparkline")
          throw new Error("Invalid chart appearance.");
        if (options.xAxis !== undefined && options.xAxis !== "sample" && options.xAxis !== "time")
          throw new Error("Invalid chart horizontal axis.");
        return createMetricChart(options, signal);
      },
    };
    return Object.freeze(ui);
  }

  // The SVG only draws geometry. Axis labels and controls use the surrounding page's font size.
  const chartWidth = 1000, chartHeight = 200;
  const chartNumber = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
  const chartScaleNumber = new Intl.NumberFormat(undefined, { maximumSignificantDigits: 5 });
  const clockSeconds = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
  const clockMillis = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit", fractionalSecondDigits: 3, hour12: false });

  function chartValue(value: number | null | undefined, unit: string): string {
    if (value == null) return "Unavailable";
    return chartNumber.format(value) + (unit && unit !== "%" ? " " : "") + unit;
  }

  function metricLine(points: readonly ExtensionChartPoint[], start: number, end: number, low: number, high: number): string {
    let path = "", coordinate = "", length = 0;
    const finish = () => {
      if (length === 1) path += `L${coordinate} `; // A round, zero-length stroke shows isolated measurements.
      length = 0;
    };
    for (const point of points) {
      if (point.time < start || point.time > end) continue;
      if (point.value === null) { finish(); continue; }
      const x = end === start ? chartWidth / 2 : chartWidth * (point.time - start) / (end - start);
      const y = chartHeight * (1 - Math.max(0, Math.min(1, (point.value - low) / (high - low))));
      coordinate = `${x.toFixed(2)} ${y.toFixed(2)}`;
      path += `${length ? "L" : "M"}${coordinate} `;
      length++;
    }
    finish();
    return path;
  }

  function chartScale(values: number[], options: ExtensionUiMetricChartOptions, fit: boolean) {
    const min = values.length ? Math.min(...values) : 0;
    const max = values.length ? Math.max(...values) : 1;
    const fixedLow = fit ? undefined : options.minimum;
    const fixedHigh = fit ? undefined : options.maximum;
    let low = fixedLow ?? (fit ? min : Math.min(0, min));
    let high = fixedHigh ?? max;
    if (high <= low) {
      const padding = Math.abs(fixedHigh ?? low) * .05 || 1;
      if (fixedHigh === undefined) high = low + padding;
      else low = high - padding;
    }
    // Round free bounds to whole tick intervals so fitting a narrow signal still gives readable labels.
    const rough = (high - low) / 4;
    const magnitude = 10 ** Math.floor(Math.log10(rough));
    const steps = [1, 2, 2.5, 5, 10];
    const step = magnitude * steps.reduce((best, candidate) =>
      Math.abs(candidate - rough / magnitude) < Math.abs(best - rough / magnitude) ? candidate : best);
    if (fixedLow === undefined) low = Math.floor(low / step) * step;
    if (fixedHigh === undefined) high = low + Math.ceil((high - low) / step) * step;
    const intervals = Math.max(2, Math.min(7, Math.round((high - low) / step) || 4));
    return { low, high, intervals };
  }

  function nearestChartTime(times: readonly number[], target: number): number | undefined {
    let low = 0, high = times.length;
    while (low < high) {
      const mid = Math.floor((low + high) / 2);
      if (times[mid]! < target) low = mid + 1;
      else high = mid;
    }
    const left = times[low - 1], right = times[low];
    return left !== undefined && (right === undefined || target - left <= right - target) ? left : right;
  }

  function fullChartFrame(element: HTMLElement, heading: HTMLElement, label: string, svg: SVGSVGElement,
    readout: HTMLElement, signal: AbortSignal, controls: {
      zoom(factor: number): void; pan(fraction: number): void; reset(): void; fit(): void;
    }) {
    const command = (text: string, title: string, action: () => void) => {
      const control = button(text, () => { if (!signal.aborted) action(); }, "plugin-chart-button");
      control.title = title;
      control.setAttribute("aria-label", title);
      return control;
    };
    const yAxis = el("div", "plugin-chart-y");
    const timeLabels = Array.from({ length: 3 }, () => el("span"));
    const empty = el("span", "plugin-chart-empty");
    const legend = el("div", "plugin-chart-legend");
    const pan = el("input", "plugin-chart-pan");
    pan.type = "range"; pan.min = "0"; pan.max = "1000"; pan.step = "1";
    pan.setAttribute("aria-label", "Position in chart history");
    const panLabel = el("label", "plugin-chart-history", el("span", "", "Timeline"), pan);

    const zoomIn = command("+", "Zoom in", () => controls.zoom(2));
    const zoomOut = command("−", "Zoom out", () => controls.zoom(.5));
    const reset = command("Reset", "Reset zoom", controls.reset);
    const scale = command("Fit values", "Fit vertical scale to visible values", controls.fit);

    const dialog = el("dialog", "plugin-chart-dialog");
    dialog.setAttribute("aria-label", label);
    const expand = button("Expand", () => {
      if (signal.aborted) return;
      if (dialog.open) { dialog.close(); return; }
      dialog.append(body);
      expand.textContent = "Close";
      expand.setAttribute("aria-label", "Close expanded chart");
      dialog.showModal();
      expand.focus();
    }, "plugin-chart-button");
    expand.setAttribute("aria-label", `Expand ${label}`);
    expand.setAttribute("aria-haspopup", "dialog");
    const body = el("div", "plugin-chart-body");
    body.append(el("div", "plugin-chart-heading", heading, expand), legend,
      el("div", "plugin-chart-axes", yAxis,
        el("div", "plugin-chart-surface", svg, empty), el("div", "plugin-chart-x", ...timeLabels)),
      el("div", "plugin-chart-footer", readout, el("div", "plugin-chart-controls", zoomIn, zoomOut, reset), scale), panLabel);
    element.append(body, dialog);
    dialog.addEventListener("close", () => {
      element.insertBefore(body, dialog);
      expand.textContent = "Expand";
      expand.setAttribute("aria-label", `Expand ${label}`);
      if (!signal.aborted && expand.isConnected) expand.focus({ preventScroll: true });
    }, { signal });
    pan.addEventListener("input", () => controls.pan(pan.valueAsNumber / 1000), { signal });
    return { dialog, yAxis, timeLabels, empty, legend, pan, panLabel, zoomIn, zoomOut, reset, scale };
  }

  function createMetricChart(options: ExtensionUiMetricChartOptions, signal: AbortSignal) {
    const sparkline = options.appearance === "sparkline";
    const heading = el("h3", "", options.label);
    const svg = chartSvg("svg", {
      viewBox: `0 0 ${chartWidth} ${chartHeight}`, preserveAspectRatio: "none", role: "img",
      class: "plugin-metric-plot", tabindex: "0", "aria-label": options.label,
    });
    const readout = el("p", "plugin-chart-readout muted");
    readout.setAttribute("aria-live", "polite");
    const latest = el("p", "plugin-chart-latest");
    const element = el("section", sparkline ? "plugin-metric-chart plugin-metric-chart-sparkline" : "plugin-metric-chart plugin-chart-full");
    const listener = new AbortController();
    if (signal.aborted) listener.abort();
    let disposed = signal.aborted;
    let series = options.series;
    const hidden = new Set<string>();
    let zoom = 1;
    let pinnedEnd: number | null = null;
    let fit = false;
    let first = 0, last = 0, start = 0, end = 0;
    let times: number[] = [];
    let maxZoom = 64;
    let yLow = 0, yHigh = 1;
    let inspected: number | undefined;

    const keys = new Map<string, { toggle: HTMLButtonElement; name: HTMLElement; value: HTMLElement; latest: string }>();
    const frame = sparkline ? null : fullChartFrame(element, heading, options.label, svg, readout, listener.signal, {
      zoom: factor => changeZoom(factor, 1),
      reset() { zoom = 1; pinnedEnd = null; draw(); },
      fit() { fit = !fit; draw(); },
      pan(fraction) {
        const span = end - start;
        panTo(first + span + fraction * (last - first - span));
      },
    });
    if (!frame) element.append(heading, latest, svg, readout);

    function panTo(nextEnd: number): void {
      pinnedEnd = nextEnd >= last ? null : Math.max(first + (last - first) / zoom, nextEnd);
      draw();
    }
    function changeZoom(factor: number, anchor: number): void {
      const next = Math.max(1, Math.min(maxZoom, zoom * factor));
      if (next === zoom || last <= first) return;
      const nextEnd = start + (end - start) * anchor + (last - first) / next * (1 - anchor);
      zoom = next;
      panTo(nextEnd);
    }

    function updateLegend(): void {
      if (!frame) return;
      const { legend } = frame;
      const ids = new Set(series.map(item => item.id));
      for (const [id, key] of keys) if (!ids.has(id)) {
        key.toggle.remove(); keys.delete(id); hidden.delete(id);
      }
      series.forEach((item, index) => {
        let key = keys.get(item.id);
        if (!key) {
          const toggle = button("", () => {
            if (disposed) return;
            if (hidden.has(item.id)) hidden.delete(item.id);
            else hidden.add(item.id);
            draw();
          }, "plugin-chart-button");
          const name = el("span", "plugin-chart-key-name"), value = el("strong");
          toggle.replaceChildren(name, value);
          key = { toggle, name, value, latest: "" };
          keys.set(item.id, key);
        }
        if (legend.children[index] !== key.toggle) legend.insertBefore(key.toggle, legend.children[index] ?? null);
        key.name.textContent = item.label;
        key.toggle.setAttribute("aria-label", item.label);
        key.toggle.title = `Show or hide ${item.label}`;
        key.toggle.dataset.series = String(index);
      });
    }

    function showInspection(): void {
      svg.querySelector(".plugin-chart-cursor-layer")?.remove();
      const time = inspected === undefined ? undefined : nearestChartTime(times, inspected);
      const single = series.length === 1 ? series[0] : undefined;
      if (single?.label === options.label) {
        const key = keys.get(single.id);
        if (key) key.name.textContent = time === undefined ? "Latest" : "Selected sample";
      }
      if (time === undefined) {
        inspected = undefined;
        for (const key of keys.values()) key.value.textContent = key.latest;
        readout.textContent = frame && times.length ? `Through ${formatTime(times.at(-1)!)}` : "";
        return;
      }
      inspected = time;
      const x = end === start ? chartWidth / 2 : chartWidth * (time - start) / (end - start);
      const cursor = chartSvg("g", { class: "plugin-chart-cursor-layer" });
      cursor.append(chartSvg("path", { class: "plugin-chart-cursor", d: `M${x} 0 V${chartHeight}`,
        stroke: "var(--muted)", "stroke-dasharray": "3 3", "vector-effect": "non-scaling-stroke" }));
      const values: string[] = [];
      series.forEach((item, index) => {
        if (hidden.has(item.id)) return;
        const point = item.points.find(point => point.time === time && point.value !== null);
        const value = chartValue(point?.value, options.unit);
        values.push(`${item.label}: ${value}`);
        const key = keys.get(item.id);
        if (key) key.value.textContent = value;
        if (frame && point?.value != null) {
          const y = chartHeight * (1 - Math.max(0, Math.min(1, (point.value - yLow) / (yHigh - yLow))));
          cursor.append(chartSvg("path", { d: `M${x} ${y}h0`, class: "plugin-chart-line", "data-series": String(index),
            stroke: "currentColor", "stroke-width": "7", "stroke-linecap": "round", "vector-effect": "non-scaling-stroke" }));
        }
      });
      svg.append(cursor);
      readout.textContent = `${formatTime(time)} · ${values.join(" · ")}`;
    }

    function formatTime(time: number): string {
      if (options.xAxis === "sample") return `#${Math.round(time).toLocaleString()}`;
      if (!Number.isFinite(new Date(time).getTime())) return String(time);
      return (end > start && end - start < 3000 ? clockMillis : clockSeconds).format(time);
    }

    function draw(): void {
      if (disposed) return;
      const starts = series.flatMap(item => item.points.length ? [item.points[0]!.time] : []);
      const ends = series.flatMap(item => item.points.length ? [item.points.at(-1)!.time] : []);
      first = starts.length ? Math.min(...starts) : 0;
      last = ends.length ? Math.max(...ends) : first;
      maxZoom = options.xAxis === "sample" ? Math.min(64, Math.max(1, last - first)) : 64;
      zoom = Math.min(zoom, maxZoom);
      const span = (last - first) / zoom;
      end = Math.max(first + span, Math.min(last, pinnedEnd ?? last)); start = end - span;
      const visible = series.filter(item => !hidden.has(item.id));
      const inWindow = (p: ExtensionChartPoint) => p.time >= start && p.time <= end;
      const points = visible.flatMap(item => item.points.filter(inWindow));
      times = [...new Set(points.map(p => p.time))].sort((a, b) => a - b);
      const values = points.flatMap(p => p.value === null ? [] : [p.value]);
      const { low, high, intervals } = chartScale(values, options, fit);
      yLow = low; yHigh = high;
      svg.replaceChildren();
      if (frame) {
        const { yAxis, timeLabels, empty, zoomIn, zoomOut, reset, panLabel, pan, scale } = frame;
        if (yAxis.children.length !== intervals + 1)
          yAxis.replaceChildren(...Array.from({ length: intervals + 1 }, () => el("span")));
        Array.from(yAxis.children).forEach((tick, i) => {
          const fraction = i / intervals;
          tick.textContent = chartScaleNumber.format(i === intervals ? low : high - (high - low) * fraction);
          svg.append(chartSvg("path", { class: "plugin-chart-gridline", d: `M0 ${chartHeight * fraction} h${chartWidth}`,
            stroke: "var(--line)", "vector-effect": "non-scaling-stroke" }));
        });
        timeLabels.forEach((label, i) => {
          label.textContent = starts.length && (span > 0 || i === 1) ? formatTime(start + span * i / 2) : "";
        });
        empty.textContent = !visible.length && series.length ? "Select a series to display" : "No readings in this window";
        empty.hidden = values.length > 0;
        svg.setAttribute("data-zoomed", String(zoom > 1));
        zoomIn.disabled = last === first || zoom >= maxZoom;
        zoomOut.disabled = reset.disabled = zoom === 1;
        panLabel.hidden = zoom === 1 || last === first;
        pan.value = String(Math.round(1000 * (start - first) / (last - first - span || 1)));
        pan.setAttribute("aria-valuetext", `${formatTime(start)} to ${formatTime(end)}`);
        scale.setAttribute("aria-pressed", String(fit));
      }
      const latestValues: string[] = [];
      series.forEach((item, index) => {
        const key = keys.get(item.id);
        let lastPoint: ExtensionChartPoint | undefined;
        for (let i = item.points.length - 1; i >= 0; i--) {
          const point = item.points[i]!;
          if (point.time < start) break;
          if (point.time <= end) { lastPoint = point; break; }
        }
        const value = chartValue(lastPoint?.value, options.unit);
        if (key) {
          key.toggle.setAttribute("aria-pressed", String(!hidden.has(item.id)));
          key.latest = value;
          key.value.textContent = value;
        }
        if (hidden.has(item.id)) return;
        latestValues.push(`${item.label}: ${value}`);
        const d = metricLine(item.points, start, end, low, high);
        svg.append(chartSvg("path", { d, class: "plugin-chart-line", "data-series": String(index),
          fill: "none", stroke: "currentColor", "stroke-width": sparkline ? "2" : "2.5", "stroke-linecap": "round",
          "stroke-linejoin": "round", "vector-effect": "non-scaling-stroke" }));
      });
      const labelled = latestValues.join(" · ") || "Unavailable";
      if (sparkline) latest.textContent = series.length === 1 ? chartValue(series[0]!.points.at(-1)?.value, options.unit) : labelled;
      svg.setAttribute("aria-label", `${options.label}. Latest: ${labelled}. ${values.length} measured values. ${zoom}× zoom. Missing values are gaps. Use arrow keys to inspect values.`);
      if (inspected !== undefined && (inspected < start || inspected > end)) inspected = undefined;
      showInspection();
    }

    const events = { signal: listener.signal };
    const pointerFraction = (event: MouseEvent) => {
      const bounds = svg.getBoundingClientRect();
      return Math.max(0, Math.min(1, (event.clientX - bounds.left) / (bounds.width || 1)));
    };
    let drag: { id: number; x: number; end: number; span: number } | undefined;
    function inspectPointer(event: PointerEvent): void {
      inspected = start + pointerFraction(event) * (end - start);
      showInspection();
    }
    svg.addEventListener("pointermove", event => {
      if (drag?.id === event.pointerId)
        panTo(drag.end - (event.clientX - drag.x) * drag.span / (svg.getBoundingClientRect().width || 1));
      inspectPointer(event);
    }, events);
    svg.addEventListener("pointerdown", event => {
      if (event.button !== 0 || !event.isPrimary) return;
      inspectPointer(event);
      if (!sparkline && zoom > 1) {
        drag = { id: event.pointerId, x: event.clientX, end, span: end - start };
        svg.setPointerCapture(event.pointerId);
      }
    }, events);
    svg.addEventListener("lostpointercapture", () => { drag = undefined; }, events);
    const clear = () => { if (!drag) { inspected = undefined; showInspection(); } };
    svg.addEventListener("pointerleave", clear, events);
    svg.addEventListener("blur", clear, events);
    svg.addEventListener("keydown", event => {
      if (!times.length || !["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
      event.preventDefault();
      const index = inspected === undefined ? -1 : times.indexOf(inspected);
      const next = event.key === "Home" ? 0 : event.key === "End" ? times.length - 1 :
        Math.max(0, Math.min(times.length - 1, index + (event.key === "ArrowLeft" ? -1 : 1)));
      inspected = times[next];
      showInspection();
    }, events);
    if (!sparkline) {
      svg.addEventListener("wheel", event => {
        if (!event.ctrlKey || event.deltaY === 0) return;
        event.preventDefault();
        changeZoom(event.deltaY < 0 ? 2 : .5, pointerFraction(event));
      }, { ...events, passive: false });
      updateLegend();
    }
    function dispose(): void {
      disposed = true;
      listener.abort();
      if (frame?.dialog.open) frame.dialog.close();
      signal.removeEventListener("abort", dispose);
    }
    signal.addEventListener("abort", dispose, { once: true });
    draw();
    return { element, setData(next: readonly ExtensionChartSeries[]) {
      validateExtensionChartSeries(next, options.xAxis);
      if (disposed) return;
      series = next;
      if (!sparkline) updateLegend();
      draw();
    }, dispose };
  }
}
