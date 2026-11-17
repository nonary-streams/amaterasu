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
  }

  export interface ExtensionUiChartsV1 {
    metricGauge(options: { label: string; value: number | null; maximum?: number; unit?: string; detail?: string }): {
      element: HTMLElement; setValue(value: number | null, detail?: string): void; dispose(): void;
    };
    metricChart(options: ExtensionUiMetricChartOptions): {
      element: HTMLElement; setData(series: readonly ExtensionChartSeries[]): void; dispose(): void;
    };
  }

  export function validateExtensionChartSeries(series: readonly ExtensionChartSeries[]): void {
    if (!Array.isArray(series) || series.length > 8 || new Set(series.map((s) => s.id)).size !== series.length)
      throw new Error("Invalid chart series.");
    for (const item of series) {
      if (!item || typeof item.id !== "string" || !item.id || typeof item.label !== "string" || !item.label ||
          !Array.isArray(item.points) || item.points.length > 2000)
        throw new Error("Invalid chart series.");
      let previous = -Infinity;
      for (const point of item.points) {
        if (!point || !Number.isFinite(point.time) || point.time < previous ||
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
        validateExtensionChartSeries(options.series);
        if (!options.label || typeof options.unit !== "string" ||
            (options.minimum !== undefined && !Number.isFinite(options.minimum)) ||
            (options.maximum !== undefined && !Number.isFinite(options.maximum)) ||
            (options.minimum !== undefined && options.maximum !== undefined && options.minimum >= options.maximum))
          throw new Error("Invalid chart scale.");
        if (options.appearance !== undefined && options.appearance !== "full" && options.appearance !== "sparkline")
          throw new Error("Invalid chart appearance.");
        const sparkline = options.appearance === "sparkline";
        const width = 640;
        const plot = sparkline
          ? { left: 4, top: 4, right: 636, bottom: 48 }
          : { left: 40, top: 10, right: 630, bottom: 150 };
        const plotWidth = plot.right - plot.left;
        const plotHeight = plot.bottom - plot.top;
        const svg = chartSvg("svg", {
          viewBox: `0 0 ${width} ${sparkline ? 52 : 180}`,
          role: "img", class: "plugin-metric-plot", tabindex: "0", "aria-label": options.label,
          ...(sparkline ? { preserveAspectRatio: "none" } : {}),
        });
        const legend = el("div", "plugin-chart-legend");
        const readout = el("p", "plugin-chart-readout muted", sparkline ? null : "Hover or use arrow keys to inspect values.");
        readout.setAttribute("aria-live", "polite");
        const controls = el("div", "plugin-chart-controls");
        const heading = el("h3", "", options.label);
        const latest = sparkline ? el("p", "plugin-chart-latest") : null;
        const element = sparkline
          ? el("section", "plugin-metric-chart plugin-metric-chart-sparkline", heading, latest, svg, readout)
          : el("section", "plugin-metric-chart", heading, legend, svg, controls, readout);
        let series = options.series;
        let disposed = signal.aborted;
        let zoom = 1;
        let inspected = 0;
        let timestamps: number[] = [];
        let domainStart = 0, domainEnd = 1;
        const hidden = new Set<string>();
        const formatter = sparkline ? new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 }) : null;
        const formatValue = (value: number | null | undefined): string => {
          if (value == null) return "Unavailable";
          if (!formatter) return `${value.toFixed(2)} ${options.unit}`;
          const number = formatter.format(value);
          if (!options.unit) return number;
          return options.unit === "%" ? `${number}%` : `${number} ${options.unit}`;
        };
        const showPoint = (time: number) => {
          if (disposed) return;
          const values = series.filter((s) => !hidden.has(s.id)).map((s) => {
            const point = s.points.reduce<ExtensionChartPoint | undefined>((best, p) => !best || Math.abs(p.time - time) < Math.abs(best.time - time) ? p : best, undefined);
            return `${s.label}: ${formatValue(point?.value)}`;
          });
          svg.querySelector(".plugin-chart-cursor")?.remove();
          const x = plot.left + plotWidth * Math.max(0, Math.min(1, (time - domainStart) / Math.max(1, domainEnd - domainStart)));
          svg.append(chartSvg("path", {
            class: "plugin-chart-cursor", d: `M ${x} ${plot.top} V ${plot.bottom}`,
            stroke: "var(--muted)", "stroke-dasharray": "3 3",
            ...(sparkline ? { "vector-effect": "non-scaling-stroke" } : {}),
          }));
          readout.textContent = `${new Date(time).toLocaleTimeString()} · ${values.join(" · ")}`;
        };
        const clearReadout = () => {
          svg.querySelector(".plugin-chart-cursor")?.remove();
          readout.textContent = "";
        };
        const draw = () => {
          if (disposed) return;
          const visible = series.filter((s) => !hidden.has(s.id));
          timestamps = [...new Set(visible.flatMap((s) => s.points.map((p) => p.time)))].sort((a, b) => a - b);
          const end = timestamps.at(-1) ?? 1;
          const begin = end - (end - (timestamps[0] ?? 0)) / zoom;
          domainStart = begin; domainEnd = end;
          timestamps = timestamps.filter(time => time >= begin);
          const points = visible.flatMap((s) => s.points.filter((p) => p.time >= begin));
          const values = points.flatMap((p) => p.value === null ? [] : [p.value]);
          const low = options.minimum ?? Math.min(0, ...values);
          const high = options.maximum ?? Math.max(low + 1, ...values);
          svg.replaceChildren();
          if (!sparkline) {
            for (const fraction of [0, .5, 1]) {
              const y = plot.bottom - plotHeight * fraction;
              svg.append(chartSvg("path", { d: `M ${plot.left} ${y} H ${plot.right}`, stroke: "var(--line)", fill: "none" }));
              const label = chartSvg("text", { x: "36", y: String(y + 4), "text-anchor": "end", fill: "currentColor", "font-size": "10" });
              label.textContent = (low + (high - low) * fraction).toFixed(1);
              svg.append(label);
            }
          }
          const strokes = sparkline
            ? ["var(--good)", "var(--accent-text)", "var(--fg)"]
            : ["var(--accent)", "var(--strong)", "var(--muted)"];
          for (const [index, item] of visible.entries()) {
            let path = "";
            let lastCoordinate = "";
            let segmentLength = 0;
            const finishSegment = () => {
              // A zero-length segment with round caps paints a dot for a single observed value.
              if (sparkline && segmentLength === 1) path += `L${lastCoordinate} `;
              segmentLength = 0;
            };
            for (const point of item.points) {
              if (point.time < begin || point.value === null) { finishSegment(); continue; }
              const x = plot.left + plotWidth * (point.time - begin) / Math.max(end - begin, 1);
              const y = plot.bottom - plotHeight * Math.max(0, Math.min(1, (point.value - low) / (high - low)));
              lastCoordinate = `${x.toFixed(2)} ${y.toFixed(2)}`;
              path += `${segmentLength ? "L" : "M"}${lastCoordinate} `;
              segmentLength++;
            }
            finishSegment();
            const dash = sparkline ? ["none", "6 2", "3 2"][Math.floor(index / 3)]! : index > 2 ? "5 3" : "none";
            svg.append(chartSvg("path", {
              d: path, fill: "none", stroke: strokes[index % strokes.length]!, "stroke-width": "2", "stroke-dasharray": dash,
              ...(sparkline ? { "stroke-linecap": "round", "stroke-linejoin": "round", "vector-effect": "non-scaling-stroke" } : {}),
            }));
          }
          const measured = points.filter((point) => point.value !== null).length;
          if (latest) {
            const values = visible.map(item => ({
              label: item.label,
              value: formatValue(item.points.at(-1)?.value),
            }));
            const labelled = values.map(item => `${item.label}: ${item.value}`).join(" · ");
            latest.textContent = values.length === 1 ? values[0]!.value : labelled || "Unavailable";
            svg.setAttribute("aria-label", `${options.label}. Latest: ${labelled || "Unavailable"}. ${measured} measured values. Missing values are gaps. Use arrow keys to inspect values.`);
            clearReadout();
          } else {
            svg.setAttribute("aria-label", `${options.label}. ${measured} measured values. ${zoom}× zoom. Missing values are gaps.`);
          }
        };
        const updateLegend = () => {
          legend.replaceChildren();
          for (const item of series) {
            const toggle = button(item.label, () => { if (hidden.has(item.id)) hidden.delete(item.id); else hidden.add(item.id); updateLegend(); draw(); });
            toggle.setAttribute("aria-pressed", String(!hidden.has(item.id)));
            legend.append(toggle);
          }
        };
        if (!sparkline) {
          for (const [label, factor] of [["Zoom in", 2], ["Zoom out", .5], ["Reset zoom", 0]] as const)
            controls.append(button(label, () => { zoom = factor ? Math.max(1, Math.min(64, zoom * factor)) : 1; draw(); }));
          updateLegend();
        }
        const onKey = (event: KeyboardEvent) => {
          if (disposed || !["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key) || !timestamps.length) return;
          event.preventDefault();
          inspected = event.key === "Home" ? 0 : event.key === "End" ? timestamps.length - 1 : Math.max(0, Math.min(timestamps.length - 1, inspected + (event.key === "ArrowLeft" ? -1 : 1)));
          showPoint(timestamps[inspected]!);
        };
        const onPointer = (event: PointerEvent) => {
          const bounds = svg.getBoundingClientRect();
          const first = timestamps[0], last = timestamps.at(-1);
          if (disposed || first === undefined || last === undefined || bounds.width <= 0) return;
          const fraction = ((event.clientX - bounds.left) / bounds.width * width - plot.left) / plotWidth;
          showPoint(domainStart + Math.max(0, Math.min(1, fraction)) * (domainEnd - domainStart));
        };
        svg.addEventListener("keydown", onKey);
        svg.addEventListener("pointermove", onPointer);
        if (sparkline) {
          svg.addEventListener("pointerleave", clearReadout);
          svg.addEventListener("blur", clearReadout);
        }
        const dispose = () => {
          disposed = true;
          signal.removeEventListener("abort", dispose);
          svg.removeEventListener("keydown", onKey);
          svg.removeEventListener("pointermove", onPointer);
          svg.removeEventListener("pointerleave", clearReadout);
          svg.removeEventListener("blur", clearReadout);
        };
        signal.addEventListener("abort", dispose, { once: true });
        draw();
        return {
          element,
          setData(next) {
            validateExtensionChartSeries(next);
            if (disposed) return;
            series = next;
            if (!sparkline) updateLegend();
            draw();
          },
          dispose,
        };
      },
    };
    return Object.freeze(ui);
  }
}
