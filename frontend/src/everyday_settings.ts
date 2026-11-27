namespace Amaterasu {
  // These existing provider options are useful on the landing page. The descriptor
  // still owns their types, choices, bounds, labels, scope and account permissions.
  export const everydayPluginOptions = new Map<string, ReadonlySet<string>>([
    ["amaterasu.frame-limiter", new Set(["frame_limiter_enable", "frame_limiter_fps_limit", "frame_limiter_auto_virtual_framegen"])],
    ["amaterasu.rtx-hdr", new Set(["rtx_hdr_scope", "rtx_hdr_enable", "rtx_hdr_peak_brightness"])],
  ]);

  /** Compact editor for the host's sparse display policy on hosts without a signed Display settings surface. */
  export function everydayDisplaySettings(policy: Record<string, unknown>) {
    const controls = new Map<string, HTMLSelectElement | HTMLInputElement>();
    function choice(key: string, title: string, help: string, options: Record<string, string>) {
      const control = el("select");
      control.name = key;
      for (const [value, text] of Object.entries({ "": "Host default", ...options })) {
        const option = el("option", "", text);
        option.value = value;
        control.append(option);
      }
      control.value = String(policy[key] ?? "");
      controls.set(key, control);
      return field(title, control, help);
    }
    function manualValue(mode: string, key: string, title: string, placeholder: string, pattern: string) {
      const select = controls.get(mode)!;
      const control = input(key, String(policy[key] ?? ""));
      control.placeholder = placeholder;
      control.pattern = pattern;
      const isResolution = key === "dd_manual_resolution";
      control.maxLength = isResolution ? 32 : 64;
      const validate = () => {
        const value = control.value;
        const inRange = isResolution
          ? value.split(/[x×]/).every(size => Number(size) > 0 && Number(size) <= 0xffffffff)
          : Number(value) > 0 && Number(value) <= 1000;
        control.setCustomValidity(value && !inRange ? (isResolution
          ? "Enter a positive width and height, such as 2560x1440."
          : "Enter a refresh rate greater than zero and up to 1000 Hz.") : "");
      };
      control.addEventListener("input", validate);
      validate();
      const row = field(title, control);
      const update = () => {
        row.hidden = select.value !== "manual";
        if (row.hidden) {
          control.value = String(policy[key] ?? "");
          validate();
        }
        control.disabled = row.hidden;
        control.required = !row.hidden;
      };
      update();
      select.addEventListener("change", update);
      controls.set(key, control);
      return row;
    }
    const node = panel("Display",
      choice("virtual_display_mode", "Streaming display", "Choose the screen Moonlight uses for new streams.", {
        disabled: "Physical display", per_client: "Virtual display per client", shared: "Shared virtual display",
      }),
      choice("dd_configuration_option", "Display preparation", "Choose which displays stay active while streaming.", {
        disabled: "Leave displays as they are", verify_only: "Verify the display", ensure_active: "Activate the streaming display",
        ensure_primary: "Make it the primary display", ensure_only_display: "Use only the streaming display",
      }),
      choice("dd_resolution_option", "Resolution", "Set the host display size when a stream starts.", {
        auto: "Match the client", disabled: "Keep the current resolution", manual: "Custom resolution",
      }),
      manualValue("dd_resolution_option", "dd_manual_resolution", "Custom resolution (width × height)", "2560x1440", "[0-9]+[x×][0-9]+"),
      choice("dd_refresh_rate_option", "Refresh rate", "Set the host display refresh rate for the stream.", {
        auto: "Match the client", prefer_highest: "Highest available", disabled: "Keep the current refresh rate", manual: "Custom refresh rate",
      }),
      manualValue("dd_refresh_rate_option", "dd_manual_refresh_rate", "Custom refresh rate (Hz)", "119.88", "[0-9]+(\\.[0-9]+)?"),
      choice("dd_hdr_option", "Display HDR", "Let the host switch HDR to match the stream, or keep the current display state.", {
        auto: "Match the stream", disabled: "Keep the current HDR state",
      }),
    );
    node.setAttribute("data-settings-owner", "amaterasu.display");
    function changes(): Record<string, string | null> {
      const patch: Record<string, string | null> = {};
      if (node.inert) return patch;
      for (const [key, control] of controls) {
        if (!control.disabled && control.value !== String(policy[key] ?? "")) patch[key] = control.value || null;
      }
      return patch;
    }
    return { node, changes };
  }
}
