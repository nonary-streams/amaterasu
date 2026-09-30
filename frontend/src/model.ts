namespace Amaterasu {
  export function applicationOutputFormValues(value: string | null | undefined): {
    output_mode: "unset" | "value";
    output_value: string;
  } {
    return {
      output_mode: value == null ? "unset" : "value",
      output_value: value ?? "",
    };
  }

  export function parseApplicationOutput(data: Pick<FormData, "get">): string | null {
    const mode = String(data.get("output_mode") ?? "unset");
    if (mode === "unset") return null;
    const value = String(data.get("output_value") ?? "");
    if (mode !== "value" || new TextEncoder().encode(value).length > 1_024 ||
        /[\u0000-\u001f\u007f-\u009f]/u.test(value))
      throw new Error("Command output must contain at most 1024 UTF-8 bytes without control characters.");
    return value;
  }

  export function parseRawPreparation(value: string): RawPreparationCommand[] {
    let parsed: unknown;
    try {
      parsed = JSON.parse(value || "[]");
    } catch {
      throw new Error("Existing raw preparation commands must be a valid JSON array.");
    }
    if (!Array.isArray(parsed) || parsed.length > 8)
      throw new Error("Existing raw preparation commands must contain at most 8 steps.");
    for (const step of parsed) {
      if (step === null || Array.isArray(step) || typeof step !== "object" ||
          Object.keys(step).some((key) => !["do", "undo", "elevated"].includes(key)))
        throw new Error("Existing raw preparation commands contain unsupported fields.");
      const command = step as Record<string, unknown>;
      for (const key of ["do", "undo"]) {
        const text = command[key];
        if (text !== undefined && (typeof text !== "string" ||
            new TextEncoder().encode(text).length > 2_048 || text.includes("\0")))
          throw new Error("Existing raw preparation commands exceed 2048 UTF-8 bytes or contain NUL.");
      }
      if (command.elevated !== undefined && typeof command.elevated !== "boolean")
        throw new Error("Existing raw preparation elevation must be a boolean.");
    }
    return parsed as RawPreparationCommand[];
  }

  const refreshRateOptionValues = ["auto", "disabled", "manual", "prefer_highest"] as const;

  export function parseRefreshRateOptionOverride(value: string): RefreshRateOption | null {
    if (value === "") return null;
    if (refreshRateOptionValues.includes(value as RefreshRateOption))
      return value as RefreshRateOption;
    throw new Error("Refresh rate option must be auto, disabled, manual, or prefer_highest.");
  }

  export function parsePrefer10BitSdrOverride(value: string): boolean | null {
    return parseInputBooleanOverride(value, "10-bit SDR preference");
  }

  export function usesManualRefreshRateCompanion(value: string | null | undefined): boolean {
    return value === "manual";
  }

  export function parseSoftwareEncoderOverrides(data: Pick<FormData, "get">): SoftwareEncoderOverrides {
    const raw = (name: string): string => String(data.get(name) ?? "");
    const option = (key: string, limit: number): string | null => {
      const mode = raw(`${key}_mode`) || "inherit";
      if (mode === "inherit") return null;
      const value = raw(`${key}_value`);
      if (mode !== "value" || new TextEncoder().encode(value).length > limit ||
          /[\u0000-\u001f\u007f-\u009f]/u.test(value))
        throw new Error(`${key} must contain at most ${limit} UTF-8 bytes without control characters.`);
      return value;
    };
    const backend = raw("encoder_backend");
    if (!["", "auto", "software"].includes(backend))
      throw new Error("Encoder backend must inherit the startup selection, auto, or software.");
    return {
      sw_preset: option("sw_preset", 128),
      sw_tune: option("sw_tune", 256),
      min_threads: parseMinThreadsOverride(raw("min_threads")),
      encoder: backend === "" ? null : backend as "auto" | "software",
    };
  }

  export function parseMinThreadsOverride(value: string): number | null {
    const trimmed = value.trim();
    if (trimmed === "") return null;
    if (!/^[1-9][0-9]{0,2}$/.test(trimmed))
      throw new Error("Minimum software encoding threads must be a whole number from 1 to 256, or blank to inherit.");
    const threads = Number(trimmed);
    if (threads > 256)
      throw new Error("Minimum software encoding threads must be a whole number from 1 to 256, or blank to inherit.");
    return threads;
  }

  export function parseArguments(value: string): string[] {
    const parsed: unknown = JSON.parse(value || "[]");
    if (
      !Array.isArray(parsed) ||
      parsed.some((argument) => typeof argument !== "string")
    ) {
      throw new Error(
        'Arguments must be a JSON array of strings, such as ["--fullscreen"].',
      );
    }
    return parsed as string[];
  }

  export function parseFecOverride(value: string): number | null {
    const trimmed = value.trim();
    if (trimmed === "") return null;
    if (!/^[0-9]+$/.test(trimmed))
      throw new Error("FEC percentage must be a whole number from 1 to 255.");
    const percentage = Number(trimmed);
    if (!Number.isSafeInteger(percentage) || percentage < 1 || percentage > 255)
      throw new Error("FEC percentage must be a whole number from 1 to 255.");
    return percentage;
  }

  export function parseBackButtonTimeoutOverride(value: string): number | null {
    const trimmed = value.trim();
    if (trimmed === "") return null;
    if (!/^-?[0-9]+$/.test(trimmed))
      throw new Error("Back button timeout must be a whole signed 32-bit number, or blank to inherit.");
    const timeout = Number(trimmed);
    if (!Number.isSafeInteger(timeout) || timeout < -0x8000_0000 || timeout > 0x7fff_ffff)
      throw new Error("Back button timeout must be a whole signed 32-bit number, or blank to inherit.");
    return timeout;
  }

  export function parseInputBooleanOverride(value: string, name: string): boolean | null {
    if (value === "") return null;
    if (value === "true") return true;
    if (value === "false") return false;
    throw new Error(`${name} must be Inherit, Enabled, or Disabled.`);
  }

  export function parseKeyRepeatDelayOverride(value: string): number | null {
    const trimmed = value.trim();
    if (trimmed === "") return null;
    if (!/^[0-9]+$/.test(trimmed))
      throw new Error("Key repeat delay must be a whole number from 0 to 2147483647 milliseconds, or blank to inherit.");
    const delay = Number(trimmed);
    if (!Number.isSafeInteger(delay) || delay > 0x7fff_ffff)
      throw new Error("Key repeat delay must be a whole number from 0 to 2147483647 milliseconds, or blank to inherit.");
    return delay;
  }

  export function parseKeyRepeatFrequencyOverride(value: string): number | null {
    const trimmed = value.trim();
    if (trimmed === "") return null;
    if (!/^(?:[+]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+))(?:[eE][+-]?[0-9]+)?$/.test(trimmed))
      throw new Error("Key repeat frequency must be a finite nonnegative number, or blank to inherit.");
    const frequency = Number(trimmed);
    if (!Number.isFinite(frequency) || frequency < 0)
      throw new Error("Key repeat frequency must be a finite nonnegative number, or blank to inherit.");
    return frequency;
  }

  export function validateKeybindings(value: unknown): number[] {
    if (!Array.isArray(value) || value.length > 512 || value.length % 2 !== 0 ||
        Array.from(value).some((byte) => typeof byte !== "number" || !Number.isInteger(byte) ||
          byte < 0 || byte > 255))
      throw new Error("Keybindings must contain at most 256 source/destination pairs of whole bytes from 0 to 255.");
    return [...value];
  }

  export function parseKeybindingsOverride(value: string): number[] | null {
    const trimmed = value.trim();
    if (trimmed === "") return null;
    if (!trimmed.startsWith("[") || !trimmed.endsWith("]"))
      throw new Error("Keybindings must be a bracketed list, such as [16, 160], or blank to inherit.");
    const contents = trimmed.slice(1, -1).trim();
    if (contents === "") return [];
    const tokens = contents.split(",").map((token) => token.trim());
    if (tokens.length > 512 || tokens.some((token) => !/^(?:[0-9]+|0[xX][0-9a-fA-F]+)$/.test(token)))
      throw new Error("Keybindings accept whole decimal or hexadecimal bytes in source/destination pairs.");
    return validateKeybindings(tokens.map((token) => Number(token)));
  }

  export function keybindingsFormValue(current?: number[] | null): string {
    return current == null ? "" : JSON.stringify(validateKeybindings(current));
  }

  export function advancedInputOverrideFormValues(current?: AdvancedInputOverrides): Record<keyof AdvancedInputOverrides, string> {
    return {
      keybindings: keybindingsFormValue(current?.keybindings),
      high_resolution_scrolling: current?.high_resolution_scrolling == null ? "" : String(current.high_resolution_scrolling),
      always_send_scancodes: current?.always_send_scancodes == null ? "" : String(current.always_send_scancodes),
      key_rightalt_to_key_win: current?.key_rightalt_to_key_win == null ? "" : String(current.key_rightalt_to_key_win),
      key_repeat_delay: current?.key_repeat_delay == null ? "" : String(current.key_repeat_delay),
      key_repeat_frequency: current?.key_repeat_frequency == null ? "" : String(current.key_repeat_frequency),
    };
  }

  export function parseAdvancedInputOverrides(data: Pick<FormData, "get">): AdvancedInputOverrides {
    const text = (name: keyof AdvancedInputOverrides) => String(data.get(name) ?? "");
    return {
      keybindings: parseKeybindingsOverride(text("keybindings")),
      high_resolution_scrolling: parseInputBooleanOverride(text("high_resolution_scrolling"), "High-resolution scrolling"),
      always_send_scancodes: parseInputBooleanOverride(text("always_send_scancodes"), "Always send scancodes"),
      key_rightalt_to_key_win: parseInputBooleanOverride(text("key_rightalt_to_key_win"), "Right Alt as Windows key"),
      key_repeat_delay: parseKeyRepeatDelayOverride(text("key_repeat_delay")),
      key_repeat_frequency: parseKeyRepeatFrequencyOverride(text("key_repeat_frequency")),
    };
  }

  export function parseGamepadProfileOverride(
    value: string,
    recognizedProfiles: readonly string[],
  ): string | null {
    if (value === "") return null;
    if (typeof value !== "string" || !recognizedProfiles.includes(value))
      throw new Error("Choose a recognized gamepad profile for this host platform.");
    return value;
  }

  export function providerConfigOverrides(value: string, streamAudioValue = "", audioSinkMode = "inherit", audioSinkId = "", audioSinkCaptureOnlyValue = "", backButtonTimeoutValue = "", virtualSinkMode = "inherit", virtualSinkSelector = "", controllerValue = "", gamepadValue = "", keyboardValue = "", mouseValue = "", recognizedGamepadProfiles: readonly string[] = ["auto"]): {
    fec_percentage: number | null;
    stream_audio: boolean | null;
    audio_sink: string | null;
    virtual_sink: string | null;
    audio_sink_capture_only: boolean | null;
    back_button_timeout: number | null;
    controller: boolean | null;
    gamepad: string | null;
    keyboard: boolean | null;
    mouse: boolean | null;
  } {
    return {
      fec_percentage: parseFecOverride(value),
      stream_audio: parseStreamAudioOverride(streamAudioValue),
      audio_sink: parseAudioSinkOverride(audioSinkMode, audioSinkId),
      virtual_sink: parseVirtualSinkOverride(virtualSinkMode, virtualSinkSelector),
      audio_sink_capture_only: parseAudioSinkCaptureOnlyOverride(audioSinkCaptureOnlyValue),
      back_button_timeout: parseBackButtonTimeoutOverride(backButtonTimeoutValue),
      controller: parseInputBooleanOverride(controllerValue, "Controller input"),
      gamepad: parseGamepadProfileOverride(gamepadValue, recognizedGamepadProfiles),
      keyboard: parseInputBooleanOverride(keyboardValue, "Keyboard input"),
      mouse: parseInputBooleanOverride(mouseValue, "Mouse input"),
    };
  }

  export function parseAudioSinkOverride(mode: string, endpoint: string): string | null {
    if (mode === "inherit") return null;
    if (mode === "automatic") return "";
    if (
      mode !== "specific" || endpoint.length === 0 ||
      new TextEncoder().encode(endpoint).length > 4096 ||
      /[\u0000-\u001f\u007f-\u009f]/.test(endpoint)
    ) throw new Error("Choose an audio endpoint ID of at most 4096 UTF-8 bytes without control characters.");
    return endpoint;
  }

  export function parseVirtualSinkOverride(mode: string, selector: string): string | null {
    if (mode === "inherit") return null;
    if (mode === "automatic") return "";
    if (
      mode !== "specific" || selector.length === 0 ||
      new TextEncoder().encode(selector).length > 4096 ||
      /[\u0000-\u001f\u007f-\u009f]/.test(selector)
    ) throw new Error("Choose a virtual sink selector of at most 4096 UTF-8 bytes without control characters.");
    return selector;
  }

  export function parseStreamAudioOverride(value: string): boolean | null {
    if (value === "") return null;
    if (value === "true") return true;
    if (value === "false") return false;
    throw new Error("Choose Inherit, Enabled, or Disabled for audio streaming.");
  }

  export function parseAudioSinkCaptureOnlyOverride(value: string): boolean | null {
    if (value === "") return null;
    if (value === "true") return true;
    if (value === "false") return false;
    throw new Error("Choose Inherit, Capture only, or Route defaults for audio output.");
  }

  export function parseSteamId(value: string): string | null {
    if (value === "") return null;
    if (!/^[1-9][0-9]*$/.test(value) || value.length > 10 || Number(value) > 0xffff_ffff)
      throw new Error("Steam app ID must be a canonical nonzero 32-bit decimal number.");
    return value;
  }

  export function parseSteamInstallDir(value: string): string | null {
    if (value === "") return null;
    if (
      value.trim() !== value ||
      new TextEncoder().encode(value).length > 1024 ||
      /[\u0000-\u001f\u007f-\u009f]/.test(value)
    ) throw new Error("Steam install directory must be a printable absolute path of at most 1024 UTF-8 bytes.");
    const normalized = value.replace(/\\/g, "/");
    let components: string[];
    if (normalized.startsWith("//")) {
      components = normalized.slice(2).split("/").filter(Boolean);
      if (components.length < 3) throw new Error("Steam UNC install directory needs a game subdirectory.");
    } else if (/^[A-Za-z]:\//.test(normalized)) {
      components = normalized.slice(3).split("/").filter(Boolean);
      if (components.length < 1) throw new Error("Steam install directory cannot be a drive root.");
    } else if (value.startsWith("/") && normalized.startsWith("/")) {
      components = normalized.slice(1).split("/").filter(Boolean);
      if (components.length < 1) throw new Error("Steam install directory cannot be a filesystem root.");
    } else {
      throw new Error("Steam install directory must be an absolute path.");
    }
    if (components.some((part) => part === "." || part === ".."))
      throw new Error("Steam install directory cannot contain dot segments.");
    return value;
  }

  export function parseDetachedCommands(value: string): string[] {
    const commands = value
      .split(/\r?\n/)
      .map((command) => command.trim())
      .filter(Boolean);
    if (
      commands.length > 16 ||
      commands.some(
        (command) =>
          new TextEncoder().encode(command).length > 2048 ||
          /[\u0000-\u001f\u007f]/.test(command),
      )
    ) {
      throw new Error(
        "Enter up to 16 detached commands, each at most 2048 bytes without control characters.",
      );
    }
    return commands;
  }

  export function parseExitTimeout(value: string): number | null {
    const trimmed = value.trim();
    if (trimmed === "") return null;
    if (!/^[0-9]+$/.test(trimmed))
      throw new Error("Exit timeout must be a whole number from 0 to 300 seconds.");
    const seconds = Number(trimmed);
    if (!Number.isSafeInteger(seconds) || seconds > 300)
      throw new Error("Exit timeout must be a whole number from 0 to 300 seconds.");
    return seconds;
  }

  export function parsePreparationSteps(value: string): PreparationStep[] {
    let parsed: unknown;
    try {
      parsed = JSON.parse(value || "[]");
    } catch {
      throw new Error("Preparation commands must be a valid JSON array.");
    }
    if (!Array.isArray(parsed) || parsed.length > 8)
      throw new Error("Preparation commands must be a JSON array with at most 8 steps.");
    for (const step of parsed) {
      if (
        step === null ||
        Array.isArray(step) ||
        typeof step !== "object" ||
        !("run" in step) ||
        Object.keys(step).some((key) => !["run", "undo"].includes(key))
      )
        throw new Error("Each preparation step must contain run and an optional undo command.");
      validatePreparationCommand(step.run);
      if (step.undo != null) validatePreparationCommand(step.undo);
    }
    return parsed as PreparationStep[];
  }

  function validatePreparationCommand(value: unknown): void {
    if (
      value === null ||
      Array.isArray(value) ||
      typeof value !== "object" ||
      !("executable" in value) ||
      Object.keys(value).some(
        (key) => !["executable", "args", "environment", "cwd", "timeout_ms"].includes(key),
      )
    )
      throw new Error("Each preparation command needs an executable and only supported fields.");
    const command = value as Record<string, unknown>;
    if (
      typeof command.executable !== "string" ||
      !isAbsolutePreparationPath(command.executable) ||
      new TextEncoder().encode(command.executable).length > 1024 ||
      command.executable.includes("\0") ||
      /\.(bat|cmd)$/i.test(command.executable)
    )
      throw new Error("Preparation executable must be a native absolute path of at most 1024 bytes; batch files need an explicit interpreter.");
    if (
      command.cwd != null &&
      (typeof command.cwd !== "string" ||
        !isAbsolutePreparationPath(command.cwd) ||
        new TextEncoder().encode(command.cwd).length > 1024 ||
        command.cwd.includes("\0"))
    )
      throw new Error("Preparation working directory must be a native absolute path of at most 1024 bytes.");
    if (command.args !== undefined) {
      if (
        !Array.isArray(command.args) ||
        command.args.length > 64 ||
        command.args.some(
          (argument) =>
            typeof argument !== "string" ||
            new TextEncoder().encode(argument).length > 1024 ||
            argument.includes("\0"),
        ) ||
        command.args.reduce(
          (bytes, argument) =>
            bytes + (typeof argument === "string" ? new TextEncoder().encode(argument).length : 0),
          0,
        ) > 8192
      )
        throw new Error("Preparation arguments exceed the 64 item and 8192 byte limits.");
    }
    if (command.timeout_ms !== undefined && (
      !Number.isSafeInteger(command.timeout_ms) ||
      (command.timeout_ms as number) < 1 ||
      (command.timeout_ms as number) > 30000
    ))
      throw new Error("Preparation timeout must be a whole number from 1 to 30000 milliseconds.");
    if (command.environment !== undefined) {
      const environment = command.environment;
      if (environment === null || Array.isArray(environment) || typeof environment !== "object")
        throw new Error("Preparation environment must be a JSON object.");
      const entries = Object.entries(environment as Record<string, unknown>);
      const foldedNames = new Set<string>();
      let totalBytes = 0;
      for (const [key, rawValue] of entries) {
        if (
          key.length === 0 ||
          new TextEncoder().encode(key).length > 128 ||
          !/^[A-Za-z0-9_]+$/.test(key) ||
          typeof rawValue !== "string" ||
          new TextEncoder().encode(rawValue).length > 1024 ||
          rawValue.includes("\0") ||
          foldedNames.has(key.toUpperCase())
        )
          throw new Error("Preparation environment names and values exceed their limits or are invalid.");
        foldedNames.add(key.toUpperCase());
        totalBytes += new TextEncoder().encode(key).length + new TextEncoder().encode(rawValue).length;
      }
      if (entries.length > 32 || totalBytes > 4096)
        throw new Error("Preparation environment exceeds the 32 variable and 4096 byte limits.");
    }
  }

  function isAbsolutePreparationPath(value: string): boolean {
    return value.startsWith("/") ||
      /^[A-Za-z]:[\\/]/.test(value) ||
      /^\\\\[^\\/]+[\\/][^\\/]+/.test(value);
  }

  function parsePreparation(value: string): PreparationStep[] {
    try {
      return parsePreparationSteps(value);
    } catch {
      throw new Error("Existing preparation commands are malformed or outside supported bounds; reload before saving.");
    }
  }

  export function entryFromForm(data: FormData, recognizedGamepadProfiles: readonly string[] = ["auto"]): Entry {
    const text = (name: string) => String(data.get(name) ?? "").trim();
    const output = parseApplicationOutput(data);
    const rawPreparation = parseRawPreparation(String(data.get("prep_cmd") ?? "[]"));
    const kind = text("kind") === "executable"
      ? "executable"
      : text("kind") === "playnite_fullscreen"
        ? "playnite_fullscreen"
        : "application";
    const fecPercentage = parseFecOverride(text("fec_percentage"));
    const streamAudio = parseStreamAudioOverride(text("stream_audio"));
    const audioSink = parseAudioSinkOverride(text("audio_sink_mode") || "inherit", String(data.get("audio_sink_id") ?? ""));
    const audioSinkCaptureOnly = parseAudioSinkCaptureOnlyOverride(text("audio_sink_capture_only"));
    const virtualSink = parseVirtualSinkOverride(text("virtual_sink_mode") || "inherit", String(data.get("virtual_sink_selector") ?? ""));
    const backButtonTimeout = parseBackButtonTimeoutOverride(text("back_button_timeout"));
    const controller = parseInputBooleanOverride(text("controller"), "Controller input");
    const gamepad = parseGamepadProfileOverride(text("gamepad"), recognizedGamepadProfiles);
    const keyboard = parseInputBooleanOverride(text("keyboard"), "Keyboard input");
    const mouse = parseInputBooleanOverride(text("mouse"), "Mouse input");
    const advancedInput = Object.fromEntries(
      Object.entries({
        ...parseAdvancedInputOverrides(data),
        ...parseSoftwareEncoderOverrides(data),
      }).filter(([, value]) => value !== null),
    );
    const refreshRateOption = parseRefreshRateOptionOverride(
      String(data.get("dd_refresh_rate_option") ?? ""),
    );
    const prefer10BitSdr = parsePrefer10BitSdrOverride(
      String(data.get("prefer_10bit_sdr") ?? ""),
    );
    const steamId = kind === "playnite_fullscreen" ? null : parseSteamId(String(data.get("steam_id") ?? ""));
    const steamInstallDir = kind === "playnite_fullscreen" ? null : parseSteamInstallDir(String(data.get("steam_install_dir") ?? ""));
    const configOverrides =
      fecPercentage === null &&
      streamAudio === null &&
      audioSink === null &&
      virtualSink === null &&
      audioSinkCaptureOnly === null &&
      backButtonTimeout === null &&
      controller === null &&
      gamepad === null &&
      keyboard === null &&
      mouse === null &&
      Object.keys(advancedInput).length === 0 &&
      refreshRateOption === null &&
      prefer10BitSdr === null
        ? undefined
        : {
            ...(fecPercentage === null ? {} : { fec_percentage: fecPercentage }),
            ...(streamAudio === null ? {} : { stream_audio: streamAudio }),
            ...(audioSink === null ? {} : { audio_sink: audioSink }),
            ...(virtualSink === null ? {} : { virtual_sink: virtualSink }),
            ...(audioSinkCaptureOnly === null ? {} : { audio_sink_capture_only: audioSinkCaptureOnly }),
            ...(backButtonTimeout === null ? {} : { back_button_timeout: backButtonTimeout }),
            ...(controller === null ? {} : { controller }),
            ...(gamepad === null ? {} : { gamepad }),
            ...(keyboard === null ? {} : { keyboard }),
            ...(mouse === null ? {} : { mouse }),
            ...advancedInput,
            ...(refreshRateOption === null ? {} : { dd_refresh_rate_option: refreshRateOption }),
            ...(prefer10BitSdr === null ? {} : { prefer_10bit_sdr: prefer10BitSdr }),
          };
    const exitTimeoutSeconds = kind === "executable" ? parseExitTimeout(text("exit_timeout_seconds")) : null;
    const detached = kind === "executable" ? parseDetachedCommands(text("detached")) : [];
    const preparation = kind === "executable" ? parsePreparation(text("preparation")) : [];
    if (preparation.length + rawPreparation.length > 8)
      throw new Error("An application can contain at most 8 preparation steps.");
    const launch: Launch = kind === "playnite_fullscreen"
      ? { kind, exit_timeout_seconds: parseExitTimeout(text("fullscreen_exit_timeout_seconds")) ?? 10 }
      : kind === "application"
        ? { kind, path: text("path") }
        : {
            kind,
            program: text("path"),
            arguments: parseArguments(String(data.get("arguments") ?? "[]")),
            working_directory: text("working_directory") || null,
            auto_detach: data.has("auto_detach"),
            wait_all: data.has("wait_all"),
            ...(exitTimeoutSeconds === null
              ? {}
              : { exit_timeout_seconds: exitTimeoutSeconds }),
            ...(detached.length ? { detached } : {}),
            ...(preparation.length ? { preparation } : {}),
          };
    return {
      name: text("name"),
      description: text("description"),
      tags: text("tags")
        .split(",")
        .map((tag) => tag.trim())
        .filter(Boolean),
      exclude_global_prep_cmd: data.has("exclude_global_prep_cmd"),
      ...(kind === "executable" ? { elevated: data.has("elevated") } : {}),
      launch,
      ...(steamId === null ? {} : { steam_id: steamId }),
      ...(steamInstallDir === null ? {} : { steam_install_dir: steamInstallDir }),
      ...(output === null ? {} : { output }),
      ...(rawPreparation.length ? { prep_cmd: rawPreparation } : {}),
      ...(configOverrides ? { config_overrides: configOverrides } : {}),
    };
  }

  export function fieldValue(
    field: Field,
    input: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement,
  ): unknown {
    if (field.type === "boolean") return (input as HTMLInputElement).checked;
    if (field.type === "string") return input.value;
    if (field.type === "enum") {
      if (!field.options.includes(input.value))
        throw new Error("Choose a listed setting value.");
      return input.value;
    }
    if (field.type === "list" || field.type === "object") {
      let value: unknown;
      try {
        value = JSON.parse(input.value);
      } catch {
        throw new Error("Enter valid JSON for this setting.");
      }
      const wrongShape =
        field.type === "list"
          ? !Array.isArray(value)
          : value === null || Array.isArray(value) || typeof value !== "object";
      if (wrongShape)
        throw new Error(
          `Enter a JSON ${field.type === "list" ? "array" : "object"} for this setting.`,
        );
      return value;
    }
    if (!input.value.trim())
      throw new Error("Numeric settings cannot be empty.");
    const value = Number(input.value);
    if (
      !Number.isFinite(value) ||
      (field.type === "integer" && !Number.isSafeInteger(value))
    )
      throw new Error(
        field.type === "integer"
          ? "Enter a whole number within the safe numeric range."
          : "Enter a finite decimal number.",
      );
    if (
      (field.minimum != null && value < field.minimum) ||
      (field.maximum != null && value > field.maximum)
    )
      throw new Error("Setting value is outside its allowed range.");
    return value;
  }

  export function launchPath(entry: Entry): string {
    switch (entry.launch.kind) {
      case "application":
        return entry.launch.path;
      case "executable":
        return entry.launch.program;
      case "playnite":
        return `Playnite game ${entry.launch.game_id}`;
      case "playnite_fullscreen":
        return "Playnite Fullscreen";
    }
  }

  export function countLabel(count: number, more: unknown): string {
    return `${count}${more != null ? "+" : ""}`;
  }
  export function messageOf(error: unknown): string {
    return error instanceof Error
      ? error.message
      : "The request could not be completed.";
  }
}
