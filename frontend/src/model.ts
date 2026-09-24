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

  export function providerConfigOverrides(value: string, streamAudioValue = ""): {
    fec_percentage: number | null;
    stream_audio: boolean | null;
  } {
    return {
      fec_percentage: parseFecOverride(value),
      stream_audio: parseStreamAudioOverride(streamAudioValue),
    };
  }

  export function parseStreamAudioOverride(value: string): boolean | null {
    if (value === "") return null;
    if (value === "true") return true;
    if (value === "false") return false;
    throw new Error("Choose Inherit, Enabled, or Disabled for audio streaming.");
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
    const kind = text("kind") === "executable" ? "executable" : "application";
    const fecPercentage = parseFecOverride(text("fec_percentage"));
    const streamAudio = parseStreamAudioOverride(text("stream_audio"));
    const configOverrides =
      fecPercentage === null && streamAudio === null
        ? undefined
        : {
            ...(fecPercentage === null ? {} : { fec_percentage: fecPercentage }),
            ...(streamAudio === null ? {} : { stream_audio: streamAudio }),
          };
    const exitTimeoutSeconds = parseExitTimeout(text("exit_timeout_seconds"));
    const detached = parseDetachedCommands(text("detached"));
    const preparation = parsePreparation(text("preparation"));
    const launch: Launch =
      kind === "application"
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
