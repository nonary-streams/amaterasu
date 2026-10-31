namespace Amaterasu {
  export type ThemePreference = "auto" | "dark" | "light";
  export type ResolvedTheme = "dark" | "light";

  export function parseThemePreference(value: unknown): ThemePreference {
    return value === "dark" || value === "light" || value === "auto" ? value : "auto";
  }

  export function resolveTheme(preference: ThemePreference, systemLight: boolean): ResolvedTheme {
    return preference === "auto" ? (systemLight ? "light" : "dark") : preference;
  }

  export class ThemeController {
    private preference: ThemePreference;
    private readonly media: MediaQueryList | null;
    private controls = new Map<ThemePreference, HTMLButtonElement>();

    constructor() {
      let saved: string | null = null;
      try { saved = localStorage.getItem("amaterasu.theme"); } catch { /* Appearance remains available without storage. */ }
      this.preference = parseThemePreference(saved);
      this.media = typeof window.matchMedia === "function" ? window.matchMedia("(prefers-color-scheme: light)") : null;
      this.media?.addEventListener("change", () => {
        if (this.preference === "auto") this.apply();
      });
      this.apply();
    }

    private apply(): void {
      document.documentElement.setAttribute("data-theme", resolveTheme(this.preference, this.media?.matches ?? false));
      for (const [value, control] of this.controls)
        control.setAttribute("aria-pressed", String(value === this.preference));
    }

    private setPreference(value: ThemePreference): void {
      this.preference = value;
      try { localStorage.setItem("amaterasu.theme", value); } catch { /* Keep the current tab's choice. */ }
      this.apply();
    }

    mount(): HTMLElement {
      this.controls.clear();
      const options = el("div", "theme-options");
      options.setAttribute("role", "group");
      setLocalizedAttribute(options, "aria-label", "Appearance");
      for (const [value, label] of [["light", "Light"], ["dark", "Dark"], ["auto", "System"]] as const) {
        const choice = button(label, () => this.setPreference(value), "theme-option");
        this.controls.set(value, choice);
        options.append(choice);
      }
      this.apply();
      return el("div", "theme-picker", el("span", "muted", "Appearance"), options);
    }
  }
}
