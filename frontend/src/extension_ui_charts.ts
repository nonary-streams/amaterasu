namespace Amaterasu {
  export interface ExtensionChartPoint { time: number; value: number | null }
  export interface ExtensionChartSeries { id: string; label: string; points: readonly ExtensionChartPoint[] }
  export interface ExtensionUiChartsV1 {
    metricGauge(options: { label: string; value: number | null; maximum?: number; unit?: string; detail?: string }): {
      element: HTMLElement; setValue(value: number | null, detail?: string): void; dispose(): void;
    };
    metricChart(options: { label: string; unit: string; series: readonly ExtensionChartSeries[]; minimum?: number; maximum?: number }): {
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
        const svg = chartSvg("svg", { viewBox: "0 0 640 180", role: "img", class: "plugin-metric-plot", tabindex: "0", "aria-label": options.label });
        const legend = el("div", "plugin-chart-legend");
        const readout = el("p", "plugin-chart-readout muted", "Hover or use arrow keys to inspect values.");
        readout.setAttribute("aria-live", "polite");
        const controls = el("div", "plugin-chart-controls");
        const element = el("section", "plugin-metric-chart", el("h3", "", options.label), legend, svg, controls, readout);
        let series = options.series;
        let disposed = signal.aborted;
        let zoom = 1;
        let inspected = 0;
        let timestamps: number[] = [];
        let domainStart = 0, domainEnd = 1;
        const hidden = new Set<string>();
        const showPoint = (time: number) => {
          if (disposed) return;
          const values = series.filter((s) => !hidden.has(s.id)).map((s) => {
            const point = s.points.reduce<ExtensionChartPoint | undefined>((best, p) => !best || Math.abs(p.time - time) < Math.abs(best.time - time) ? p : best, undefined);
            return `${s.label}: ${point?.value == null ? "Unavailable" : `${point.value.toFixed(2)} ${options.unit}`}`;
          });
          svg.querySelector(".plugin-chart-cursor")?.remove();
          const x = 40 + 590 * Math.max(0,Math.min(1,(time-domainStart)/Math.max(1,domainEnd-domainStart)));
          svg.append(chartSvg("path",{class:"plugin-chart-cursor",d:`M ${x} 10 V 150`,stroke:"var(--muted)","stroke-dasharray":"3 3"}));
          readout.textContent = `${new Date(time).toLocaleTimeString()} · ${values.join(" · ")}`;
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
          for (const fraction of [0, .5, 1]) {
            const y = 150 - 140 * fraction;
            svg.append(chartSvg("path", { d: `M 40 ${y} H 630`, stroke: "var(--line)", fill: "none" }));
            const label = chartSvg("text", { x: "36", y: String(y + 4), "text-anchor": "end", fill: "currentColor", "font-size": "10" });
            label.textContent = (low + (high - low) * fraction).toFixed(1);
            svg.append(label);
          }
          for (const [index, item] of visible.entries()) {
            let path = "";
            let connected = false;
            for (const point of item.points) {
              if (point.time < begin || point.value === null) { connected = false; continue; }
              const x = 40 + 590 * (point.time - begin) / Math.max(end - begin, 1);
              const y = 150 - 140 * Math.max(0, Math.min(1, (point.value - low) / (high - low)));
              path += `${connected ? "L" : "M"}${x.toFixed(2)} ${y.toFixed(2)} `;
              connected = true;
            }
            svg.append(chartSvg("path", { d: path, fill: "none", stroke: ["var(--accent)", "var(--strong)", "var(--muted)"][index % 3]!, "stroke-width": "2", "stroke-dasharray": index > 2 ? "5 3" : "none" }));
          }
          svg.setAttribute("aria-label", `${options.label}. ${points.filter((p) => p.value !== null).length} measured values. ${zoom}× zoom. Missing values are gaps.`);
        };
        const updateLegend = () => {
          legend.replaceChildren();
          for (const item of series) {
            const toggle = button(item.label, () => { if (hidden.has(item.id)) hidden.delete(item.id); else hidden.add(item.id); updateLegend(); draw(); });
            toggle.setAttribute("aria-pressed", String(!hidden.has(item.id)));
            legend.append(toggle);
          }
        };
        for (const [label, factor] of [["Zoom in", 2], ["Zoom out", .5], ["Reset zoom", 0]] as const)
          controls.append(button(label, () => { zoom = factor ? Math.max(1, Math.min(64, zoom * factor)) : 1; draw(); }));
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
          showPoint(domainStart + Math.max(0, Math.min(1, ((event.clientX - bounds.left) / bounds.width * 640 - 40) / 590)) * (domainEnd - domainStart));
        };
        svg.addEventListener("keydown", onKey);
        svg.addEventListener("pointermove", onPointer);
        const dispose = () => { disposed = true; signal.removeEventListener("abort", dispose); svg.removeEventListener("keydown", onKey); svg.removeEventListener("pointermove", onPointer); };
        signal.addEventListener("abort", dispose, { once: true });
        updateLegend(); draw();
        return { element, setData(next) { validateExtensionChartSeries(next); if (!disposed) { series = next; updateLegend(); draw(); } }, dispose };
      },
    };
    return Object.freeze(ui);
  }
}
