namespace Amaterasu {
  export type ExtensionUiInfoIcon = "gpu" | "capture" | "encoder" | "catalog" | "sessions" | "processes" | "discoveries";

  export interface ExtensionUiStatusCardOptions<T = ExtensionUiText> {
    title: T;
    label?: T;
    description?: T;
    tone?: ExtensionUiBadgeTone;
  }

  export interface ExtensionUiInfoCardOptions<T = ExtensionUiText> {
    label: T;
    value: T;
    description?: T;
    icon?: ExtensionUiInfoIcon;
  }

  export interface ExtensionUiSummaryV1 {
    readonly version: 1;
    statusCard(options: ExtensionUiStatusCardOptions): ExtensionUiElement;
    infoCard(options: ExtensionUiInfoCardOptions): ExtensionUiElement;
  }

  // Compound, even-odd silhouettes keep the small pictograms legible on either paper tone.
  const infoIconPaths: Record<ExtensionUiInfoIcon, string> = {
    gpu: "M5 2h14v2h3v2h-3v2h3v2h-3v2h3v2h-3v2h3v2h-3v3H5v-3H2v-2h3v-2H2v-2h3v-2H2V8h3V6H2V4h3z M7 5v13h10V5z M8 6h8v11H8z M13.8 9l-3.3 3H9l3.1-3z M15 11v1.8l-2.5 2.4H11z",
    capture: "M2 3h20v14H2z M4 5v10h16V5z M5 14l10-7-2.5 4.6 5.2.4-3 2z M10 17h4l.7 2.6 4.3.5V22H5v-1.9l4.3-.5z",
    encoder: "M2 4.3 20.8.8l.8 4.6L2.7 9z M4.5 4.5l2 2.5 2.6-.5-2-2.5z M11.8 3.1l2 2.5 2.5-.5-2-2.4z M3 9.7h18V21H3z M10 12v6l5.6-3.1z",
    catalog: "M2 5a2 2 0 0 1 2-2h6l2.2 2H20a2 2 0 0 1 2 2v2H2z M2 10h20l-2 11H4z M7 13v2h9v-2z",
    sessions: "M2 5h13V2l6 5-6 5V9H2z M22 15H9v-3l-6 5 6 5v-3h13z",
    processes: "M2 3h20v15H2z M4 5v11h16V5z M5 11h3l2-4 3 5 1-2h5v2h-4l-2 3-3-5-1 3H5z M10 18h4v2h4v2H6v-2h4z",
    discoveries: "M12 1.5a10.5 10.5 0 1 1 0 21 10.5 10.5 0 1 1 0-21z M12 4a8 8 0 1 0 0 16 8 8 0 0 0 0-16z M17.4 6.6l-3.8 7-7 3.8 3.8-7z M12 11a1 1 0 1 0 0 2 1 1 0 0 0 0-2z",
  };

  const statusGlyphPaths: Record<ExtensionUiBadgeTone, string> = {
    success: "M3.5 12.8 6 10.1l4.1 3.7 8.8-9.4 2.2 2.5-10.8 13z",
    warning: "M10 4.2h4L13.2 14h-2.4z M10.4 16h3.2v3.6h-3.2z",
    error: "M5.2 7.4l2.2-2.2 4.6 4.6 4.6-4.6 2.2 2.2-4.6 4.6 4.6 4.6-2.2 2.2-4.6-4.6-4.6 4.6-2.2-2.2L9.8 12z",
    info: "M10.1 4h3.8v3.8h-3.8z M10.4 10h3.2v9.9h-3.2z",
    neutral: "M5 10.4h14v3.2H5z",
  };
  let nextSummaryHeading = 0;

  function summaryPictogram(data: string): SVGSVGElement {
    const ns = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(ns, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("focusable", "false");
    const path = document.createElementNS(ns, "path");
    path.setAttribute("fill", "currentColor");
    path.setAttribute("fill-rule", "evenodd");
    path.setAttribute("d", data);
    svg.append(path);
    return svg;
  }

  export function summaryStatusCard(options: ExtensionUiStatusCardOptions<TextValue | Text>): HTMLElement {
    const tone = options.tone ?? "neutral";
    if (typeof tone !== "string" || !Object.hasOwn(statusGlyphPaths, tone)) throw new Error("Invalid status card tone.");
    const title = Amaterasu.el("h2", "ink-status-title", options.title);
    title.id = `amaterasu-summary-heading-${++nextSummaryHeading}`;
    const copy = Amaterasu.el("div", "ink-summary-copy", title);
    if (options.description !== undefined) copy.append(Amaterasu.el("p", "ink-summary-description", options.description));
    const seal = Amaterasu.el("span", "ink-seal ink-status-seal", summaryPictogram(statusGlyphPaths[tone]));
    seal.setAttribute("aria-hidden", "true");
    const card = Amaterasu.el("section", "ink-status-card");
    card.setAttribute("aria-labelledby", title.id);
    card.setAttribute("data-summary-tone", tone);
    if (options.label !== undefined) card.append(Amaterasu.el("p", "ink-summary-label", options.label));
    card.append(Amaterasu.el("div", "ink-status-body", seal, copy));
    return card;
  }

  export function summaryInfoCard(options: ExtensionUiInfoCardOptions<TextValue | Text>): HTMLElement {
    const card = Amaterasu.el("div", "ink-info-card");
    if (options.icon !== undefined) {
      if (typeof options.icon !== "string" || !Object.hasOwn(infoIconPaths, options.icon)) throw new Error("Invalid info card icon.");
      card.append(Amaterasu.el("span", "ink-info-icon", summaryPictogram(infoIconPaths[options.icon])));
    }
    const copy = Amaterasu.el("div", "ink-summary-copy",
      Amaterasu.el("span", "ink-summary-label", options.label),
      Amaterasu.el("strong", "ink-info-value", options.value));
    if (options.description !== undefined) copy.append(Amaterasu.el("small", "ink-summary-description", options.description));
    card.append(copy);
    return card;
  }

  export function createExtensionUiSummaryV1(signal: AbortSignal): ExtensionUiSummaryV1 {
    if (!signal || typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function")
      throw new Error("Plugin UI requires a lifecycle signal.");
    Amaterasu.ensureExtensionUiElementsActive(signal);
    const optionalText = (value?: ExtensionUiText) => value === undefined ? undefined : Amaterasu.extensionUiTextNode(value);
    const api: ExtensionUiSummaryV1 = {
      version: 1,
      statusCard(options) {
        Amaterasu.ensureExtensionUiElementsActive(signal);
        const card = summaryStatusCard({
          title: Amaterasu.extensionUiTextNode(options.title),
          label: optionalText(options.label),
          description: optionalText(options.description),
          tone: options.tone,
        });
        return Amaterasu.disposableExtensionUiElement(card, signal);
      },
      infoCard(options) {
        Amaterasu.ensureExtensionUiElementsActive(signal);
        const card = summaryInfoCard({
          label: Amaterasu.extensionUiTextNode(options.label),
          value: typeof options.value === "string" ? document.createTextNode(options.value) : Amaterasu.extensionUiTextNode(options.value),
          description: optionalText(options.description),
          icon: options.icon,
        });
        return Amaterasu.disposableExtensionUiElement(card, signal);
      },
    };
    return Object.freeze(api);
  }
}
