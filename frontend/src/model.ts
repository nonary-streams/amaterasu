namespace Amaterasu {
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

  export function providerConfigOverrides(value: string, streamAudioValue = "", audioSinkMode = "inherit", audioSinkId = "", audioSinkCaptureOnlyValue = ""): {
    fec_percentage: number | null;
    stream_audio: boolean | null;
    audio_sink: string | null;
    audio_sink_capture_only: boolean | null;
  } {
    return {
      fec_percentage: parseFecOverride(value),
      stream_audio: parseStreamAudioOverride(streamAudioValue),
      audio_sink: parseAudioSinkOverride(audioSinkMode, audioSinkId),
      audio_sink_capture_only: parseAudioSinkCaptureOnlyOverride(audioSinkCaptureOnlyValue),
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

  function parsePreparation(value: string): PreparationStep[] {
    const parsed: unknown = JSON.parse(value || "[]");
    if (
      !Array.isArray(parsed) ||
      parsed.some(
        (step) =>
          step === null ||
          Array.isArray(step) ||
          typeof step !== "object" ||
          !step.run ||
          Array.isArray(step.run) ||
          typeof step.run !== "object",
      )
    ) {
      throw new Error("Existing preparation commands are malformed; reload before saving.");
    }
    return parsed as PreparationStep[];
  }

  export function entryFromForm(data: FormData): Entry {
    const text = (name: string) => String(data.get(name) ?? "").trim();
    const kind = text("kind") === "executable"
      ? "executable"
      : text("kind") === "playnite_fullscreen"
        ? "playnite_fullscreen"
        : "application";
    const fecPercentage = parseFecOverride(text("fec_percentage"));
    const streamAudio = parseStreamAudioOverride(text("stream_audio"));
    const audioSink = parseAudioSinkOverride(text("audio_sink_mode") || "inherit", String(data.get("audio_sink_id") ?? ""));
    const audioSinkCaptureOnly = parseAudioSinkCaptureOnlyOverride(text("audio_sink_capture_only"));
    const steamId = kind === "playnite_fullscreen" ? null : parseSteamId(String(data.get("steam_id") ?? ""));
    const steamInstallDir = kind === "playnite_fullscreen" ? null : parseSteamInstallDir(String(data.get("steam_install_dir") ?? ""));
    const configOverrides =
      fecPercentage === null && streamAudio === null && audioSink === null && audioSinkCaptureOnly === null
        ? undefined
        : {
            ...(fecPercentage === null ? {} : { fec_percentage: fecPercentage }),
            ...(streamAudio === null ? {} : { stream_audio: streamAudio }),
            ...(audioSink === null ? {} : { audio_sink: audioSink }),
            ...(audioSinkCaptureOnly === null ? {} : { audio_sink_capture_only: audioSinkCaptureOnly }),
          };
    const exitTimeoutSeconds = kind === "executable" ? parseExitTimeout(text("exit_timeout_seconds")) : null;
    const detached = kind === "executable" ? parseDetachedCommands(text("detached")) : [];
    const preparation = kind === "executable" ? parsePreparation(text("preparation")) : [];
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
      launch,
      ...(steamId === null ? {} : { steam_id: steamId }),
      ...(steamInstallDir === null ? {} : { steam_install_dir: steamInstallDir }),
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
