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

  export function entryFromForm(data: FormData): Entry {
    const text = (name: string) => String(data.get(name) ?? "").trim();
    const kind = text("kind") === "executable" ? "executable" : "application";
    const launch: Launch =
      kind === "application"
        ? { kind, path: text("path") }
        : {
            kind,
            program: text("path"),
            arguments: parseArguments(String(data.get("arguments") ?? "[]")),
            working_directory: text("working_directory") || null,
          };
    return {
      name: text("name"),
      description: text("description"),
      tags: text("tags")
        .split(",")
        .map((tag) => tag.trim())
        .filter(Boolean),
      launch,
    };
  }

  export function fieldValue(
    field: Field,
    input: HTMLInputElement,
  ): boolean | number | string {
    if (field.type === "boolean") return input.checked;
    if (field.type === "string") return input.value;
    if (!input.value.trim())
      throw new Error("Integer settings cannot be empty.");
    const value = Number(input.value);
    if (!Number.isSafeInteger(value))
      throw new Error("Enter a whole number within the safe numeric range.");
    return value;
  }

  export function launchPath(entry: Entry): string {
    return entry.launch.kind === "application"
      ? (entry.launch.path ?? "")
      : (entry.launch.program ?? "");
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
