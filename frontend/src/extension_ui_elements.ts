namespace Amaterasu {
  export type ExtensionUiBadgeTone = "neutral" | "success" | "warning" | "error" | "info";

  export interface ExtensionUiBadgeOptions {
    label: ExtensionUiText;
    tone?: ExtensionUiBadgeTone;
  }

  export interface ExtensionUiTooltipOptions {
    trigger: HTMLElement;
    message: ExtensionUiText;
  }

  export interface ExtensionUiElement {
    readonly element: HTMLElement;
    dispose(): void;
  }

  export interface ExtensionUiElementsV1 {
    readonly version: 1;
    badge(options: ExtensionUiBadgeOptions): ExtensionUiElement;
    tooltip(options: ExtensionUiTooltipOptions): ExtensionUiElement;
  }

  const extensionUiBadgeTones: readonly ExtensionUiBadgeTone[] = ["neutral", "success", "warning", "error", "info"];
  const extensionUiTooltipTriggers = new WeakSet<HTMLButtonElement>();
  let nextExtensionUiTooltipId = 0;

  export function ensureExtensionUiElementsActive(signal: AbortSignal): void {
    if (signal.aborted) throw new Error("Plugin page is no longer active.");
  }

  export function disposableExtensionUiElement(
    element: HTMLElement,
    signal: AbortSignal,
    release?: () => void,
  ): ExtensionUiElement {
    let disposed = false;
    const dispose = (): void => {
      if (disposed) return;
      disposed = true;
      signal.removeEventListener("abort", dispose);
      release?.();
      element.remove();
    };
    signal.addEventListener("abort", dispose, { once: true });
    return Object.freeze({ element, dispose });
  }

  function extensionUiTooltipButton(trigger: HTMLElement): HTMLButtonElement {
    if (trigger instanceof HTMLButtonElement) return trigger;
    if (trigger instanceof HTMLElement) {
      const buttons = trigger.querySelectorAll("button");
      if (buttons.length === 1 && buttons[0] instanceof HTMLButtonElement) return buttons[0];
    }
    throw new Error("A plugin tooltip trigger must be a button or contain exactly one button.");
  }

  function extensionUiDescriptionIds(button: HTMLButtonElement): string[] {
    return (button.getAttribute("aria-describedby") ?? "").trim().split(/\s+/).filter(Boolean);
  }

  function positionExtensionUiTooltip(anchor: HTMLElement, tooltip: HTMLElement): void {
    const viewport = anchor.ownerDocument.documentElement;
    const rect = anchor.getBoundingClientRect();
    const padding = 8;
    const rightmost = viewport.clientWidth - tooltip.offsetWidth - padding;
    const left = Math.max(padding, Math.min(rect.left, rightmost));
    tooltip.style.setProperty("--plugin-tooltip-offset", `${left - rect.left}px`);
    const height = tooltip.offsetHeight;
    tooltip.dataset.placement = rect.bottom + height > viewport.clientHeight - padding && rect.top >= height + padding
      ? "above" : "below";
  }

  function createExtensionUiTooltip(signal: AbortSignal, options: ExtensionUiTooltipOptions): ExtensionUiElement {
    const { trigger } = options;
    const button = extensionUiTooltipButton(trigger);
    if (extensionUiTooltipTriggers.has(button)) throw new Error("The plugin tooltip trigger already has a tooltip.");

    const tooltip = Amaterasu.el("span", "plugin-tooltip", Amaterasu.extensionUiTextNode(options.message));
    tooltip.id = `amaterasu-plugin-tooltip-${++nextExtensionUiTooltipId}`;
    tooltip.hidden = true;
    tooltip.setAttribute("role", "tooltip");
    const anchor = Amaterasu.el("div", "plugin-tooltip-anchor", trigger, tooltip);
    const document = button.ownerDocument;
    const listeners = new AbortController();
    extensionUiTooltipTriggers.add(button);

    const describeTrigger = (): void => {
      const ids = extensionUiDescriptionIds(button);
      if (!ids.includes(tooltip.id)) button.setAttribute("aria-describedby", [...ids, tooltip.id].join(" "));
    };
    describeTrigger();
    // The action SDK can replace its own help/error description IDs while the tooltip is mounted.
    const descriptions = new MutationObserver(describeTrigger);
    descriptions.observe(button, { attributes: true, attributeFilter: ["aria-describedby"] });

    let hovered = false;
    let focused = false;
    let dismissed = false;
    const position = (): void => positionExtensionUiTooltip(anchor, tooltip);
    const updateVisibility = (): void => {
      const visible = !dismissed && (hovered || focused);
      if (visible === !tooltip.hidden) return;
      tooltip.hidden = !visible;
      if (visible) {
        position();
        document.addEventListener("keydown", dismiss, { capture: true, signal: listeners.signal });
        document.addEventListener("scroll", position, { capture: true, passive: true, signal: listeners.signal });
        document.defaultView?.addEventListener("resize", position, { signal: listeners.signal });
      } else {
        document.removeEventListener("keydown", dismiss, true);
        document.removeEventListener("scroll", position, true);
        document.defaultView?.removeEventListener("resize", position);
      }
    };
    const dismiss = (event: KeyboardEvent): void => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      dismissed = true;
      updateVisibility();
    };

    const listenerOptions = { signal: listeners.signal };
    anchor.addEventListener("pointerenter", (event) => {
      if (event.pointerType === "touch") return;
      hovered = true;
      dismissed = false;
      updateVisibility();
    }, listenerOptions);
    anchor.addEventListener("pointerleave", () => {
      hovered = false;
      updateVisibility();
    }, listenerOptions);
    button.addEventListener("focus", () => {
      focused = true;
      dismissed = false;
      updateVisibility();
    }, listenerOptions);
    button.addEventListener("blur", () => {
      focused = false;
      updateVisibility();
    }, listenerOptions);

    return disposableExtensionUiElement(anchor, signal, () => {
      listeners.abort();
      descriptions.disconnect();
      const ids = extensionUiDescriptionIds(button).filter((id) => id !== tooltip.id);
      if (ids.length) button.setAttribute("aria-describedby", ids.join(" "));
      else button.removeAttribute("aria-describedby");
      extensionUiTooltipTriggers.delete(button);
      tooltip.hidden = true;
      anchor.replaceChildren();
    });
  }

  export function createExtensionUiElementsV1(signal: AbortSignal): ExtensionUiElementsV1 {
    if (!signal || typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function")
      throw new Error("Plugin UI requires a lifecycle signal.");
    ensureExtensionUiElementsActive(signal);
    const elements: ExtensionUiElementsV1 = {
      version: 1,
      badge(options) {
        ensureExtensionUiElementsActive(signal);
        if (!options || typeof options !== "object" || Array.isArray(options)) throw new Error("Invalid plugin badge.");
        const tone = options.tone === undefined ? "neutral" : options.tone;
        if (!extensionUiBadgeTones.includes(tone)) throw new Error("Invalid plugin badge tone.");
        const element = Amaterasu.el("span", "badge plugin-badge", Amaterasu.extensionUiTextNode(options.label));
        element.dataset.tone = tone;
        return disposableExtensionUiElement(element, signal);
      },
      tooltip(options) {
        ensureExtensionUiElementsActive(signal);
        if (!options || typeof options !== "object" || Array.isArray(options)) throw new Error("Invalid plugin tooltip.");
        return createExtensionUiTooltip(signal, options);
      },
    };
    return Object.freeze(elements);
  }
}
