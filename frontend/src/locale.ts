namespace Amaterasu {
  export const LOCALE_OPTIONS = [
    "bg",
    "cs",
    "de",
    "en",
    "en_GB",
    "en_US",
    "es",
    "fr",
    "hu",
    "it",
    "ja",
    "ko",
    "pl",
    "pt",
    "pt_BR",
    "ru",
    "sv",
    "tr",
    "uk",
    "vi",
    "zh",
    "zh_TW",
  ] as const;

  export type LocaleToken = (typeof LOCALE_OPTIONS)[number];

  export interface LocaleConfigurationDocument {
    revision: number;
    fields: Record<string, unknown>;
    values?: Record<string, unknown>;
    [key: string]: unknown;
  }

  function isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === "object" && value !== null && !Array.isArray(value);
  }

  function localeFieldDefinition(): Record<string, unknown> {
    return {
      type: "enum",
      default: "en",
      options: [...LOCALE_OPTIONS],
    };
  }

  export function validateLocale(value: unknown): LocaleToken {
    if (
      typeof value !== "string" ||
      !(LOCALE_OPTIONS as readonly string[]).includes(value)
    ) {
      throw new Error("The locale value is not one of the supported tokens.");
    }
    return value as LocaleToken;
  }

  export function validateLocaleDefinition(definition: unknown): void {
    if (!isRecord(definition) || definition.type !== "enum") {
      throw new Error("The registered locale field must be an enum.");
    }
    if (definition.default !== "en") {
      throw new Error("The registered locale field must default to en.");
    }
    if (
      !Array.isArray(definition.options) ||
      definition.options.length !== LOCALE_OPTIONS.length ||
      !LOCALE_OPTIONS.every(
        (locale, index) => definition.options[index] === locale,
      )
    ) {
      throw new Error("The registered locale options are incompatible.");
    }
  }

  function validateConfigurationDocument(
    configuration: unknown,
  ): LocaleConfigurationDocument {
    if (
      !isRecord(configuration) ||
      !Number.isSafeInteger(configuration.revision) ||
      !isRecord(configuration.fields) ||
      (configuration.values !== undefined && !isRecord(configuration.values))
    ) {
      throw new Error(
        "The configuration service returned an invalid document.",
      );
    }
    return configuration as LocaleConfigurationDocument;
  }

  export function localeFromConfiguration(configuration: unknown): LocaleToken {
    const document = validateConfigurationDocument(configuration);
    if (!Object.prototype.hasOwnProperty.call(document.fields, "locale")) {
      throw new Error(
        "The registered configuration has no locale field.",
      );
    }
    const definition = document.fields.locale;
    validateLocaleDefinition(definition);
    const values = document.values ?? {};
    const value = Object.prototype.hasOwnProperty.call(values, "locale")
      ? values.locale
      : (definition as Record<string, unknown>).default;
    return validateLocale(value);
  }

  export function extendLocaleConfiguration(
    configuration: unknown,
  ): LocaleConfigurationDocument {
    const document = validateConfigurationDocument(configuration);
    if (Object.prototype.hasOwnProperty.call(document.fields, "locale")) {
      throw new Error("The registered locale field cannot be replaced.");
    }
    return {
      ...document,
      fields: {
        ...document.fields,
        locale: localeFieldDefinition(),
      },
      values: {
        ...(document.values ?? {}),
      },
    };
  }

  export function documentLanguage(value: unknown): string {
    return validateLocale(value).replaceAll("_", "-");
  }

  function errorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
  }

  function errorCode(error: unknown): string | null {
    if (!isRecord(error) || typeof error.code !== "string") return null;
    return error.code;
  }

  async function getLocaleConfiguration(
    api: Api,
  ): Promise<LocaleConfigurationDocument> {
    try {
      return validateConfigurationDocument(
        await api.post<LocaleConfigurationDocument>("configuration/get"),
      );
    } catch (error) {
      if (errorCode(error) !== "not_registered") throw error;
      if (!api.allows("configuration.write")) {
        throw new Error(
          "Locale setup requires configuration.write access.",
        );
      }
      await api.post("configuration/register", {
        fields: { locale: localeFieldDefinition() },
      });
      return validateConfigurationDocument(
        await api.post<LocaleConfigurationDocument>("configuration/get"),
      );
    }
  }

  async function ensureLocaleConfiguration(
    api: Api,
  ): Promise<LocaleConfigurationDocument> {
    if (!api.allows("configuration.read")) {
      throw new Error(
        "Locale preferences require configuration.read access.",
      );
    }
    let configuration = await getLocaleConfiguration(api);
    if (!Object.prototype.hasOwnProperty.call(configuration.fields, "locale")) {
      if (!api.allows("configuration.write")) {
        throw new Error(
          "Adding the locale preference requires configuration.write access.",
        );
      }
      const extended = extendLocaleConfiguration(configuration);
      await api.post("configuration/migrate", {
        fields: extended.fields,
        expected_revision: configuration.revision,
      });
      configuration = validateConfigurationDocument(
        await api.post<LocaleConfigurationDocument>("configuration/get"),
      );
    }
    localeFromConfiguration(configuration);
    return configuration;
  }

  export function mountLocaleSelector(api: Api): HTMLElement {
    const container = document.createElement("div");
    container.className = "locale-control";

    const label = document.createElement("label");
    label.htmlFor = "global-locale";
    label.textContent = "Locale";
    label.title = "Stored locale preference. Console text remains English.";

    const select = document.createElement("select");
    select.id = "global-locale";
    select.setAttribute("aria-label", "Locale");
    for (const locale of LOCALE_OPTIONS) {
      const option = document.createElement("option");
      option.value = locale;
      option.textContent = locale;
      select.append(option);
    }
    select.disabled = true;

    const status = document.createElement("span");
    status.className = "locale-status";
    status.setAttribute("role", "status");
    const error = document.createElement("span");
    error.className = "locale-error";
    error.setAttribute("role", "alert");
    const note = document.createElement("small");
    note.textContent = "Console text remains English.";
    container.append(label, select, status, error, note);

    const canWrite = api.allows("configuration.write");
    let busy = false;
    let loaded = false;
    let revision: number | null = null;

    const updateDisabledState = (): void => {
      select.disabled = busy || !canWrite || !loaded;
    };
    const showError = (message: string): void => {
      status.textContent = "";
      error.textContent = message;
    };
    const clearError = (): void => {
      error.textContent = "";
    };

    const load = async (previousError?: string): Promise<void> => {
      busy = true;
      updateDisabledState();
      status.textContent = "Loading locale preference…";
      clearError();
      try {
        const configuration = await ensureLocaleConfiguration(api);
        const locale = localeFromConfiguration(configuration);
        select.value = locale;
        document.documentElement.lang = documentLanguage(locale);
        revision = configuration.revision;
        loaded = true;
        if (previousError) showError(previousError);
        else if (!canWrite) status.textContent = "Read only";
        else status.textContent = "";
      } catch (loadError) {
        loaded = false;
        revision = null;
        showError(
          previousError
            ? `${previousError} Reload failed: ${errorMessage(loadError)}`
            : `Unable to load locale preference: ${errorMessage(loadError)}`,
        );
      } finally {
        busy = false;
        updateDisabledState();
      }
    };

    select.addEventListener("change", () => {
      if (!loaded || !canWrite || revision === null) return;
      let locale: LocaleToken;
      try {
        locale = validateLocale(select.value);
      } catch (validationError) {
        showError(errorMessage(validationError));
        return;
      }
      busy = true;
      updateDisabledState();
      status.textContent = "Saving locale preference…";
      clearError();
      void api
        .post("configuration/update", {
          values: { locale },
          expected_revision: revision,
        })
        .then(() => load())
        .catch((saveError: unknown) =>
          load(`Unable to save locale preference: ${errorMessage(saveError)}`),
        );
    });

    if (!api.allows("configuration.read")) {
      showError("Locale preferences require configuration.read access.");
    } else {
      void load();
    }
    updateDisabledState();
    return container;
  }
}
