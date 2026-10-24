"use strict";
var Amaterasu;
(function (Amaterasu) {
    const refreshRateOptionValues = ["auto", "disabled", "manual", "prefer_highest"];
    function parseRefreshRateOptionOverride(value) {
        if (value === "")
            return null;
        if (refreshRateOptionValues.includes(value))
            return value;
        throw new Error("Refresh rate option must be auto, disabled, manual, or prefer_highest.");
    }
    Amaterasu.parseRefreshRateOptionOverride = parseRefreshRateOptionOverride;
    function parsePrefer10BitSdrOverride(value) {
        return parseInputBooleanOverride(value, "10-bit SDR preference");
    }
    Amaterasu.parsePrefer10BitSdrOverride = parsePrefer10BitSdrOverride;
    function usesManualRefreshRateCompanion(value) {
        return value === "manual";
    }
    Amaterasu.usesManualRefreshRateCompanion = usesManualRefreshRateCompanion;
    function parseArguments(value) {
        const parsed = JSON.parse(value || "[]");
        if (!Array.isArray(parsed) ||
            parsed.some((argument) => typeof argument !== "string")) {
            throw new Error('Arguments must be a JSON array of strings, such as ["--fullscreen"].');
        }
        return parsed;
    }
    Amaterasu.parseArguments = parseArguments;
    function parseFecOverride(value) {
        const trimmed = value.trim();
        if (trimmed === "")
            return null;
        if (!/^[0-9]+$/.test(trimmed))
            throw new Error("FEC percentage must be a whole number from 1 to 255.");
        const percentage = Number(trimmed);
        if (!Number.isSafeInteger(percentage) || percentage < 1 || percentage > 255)
            throw new Error("FEC percentage must be a whole number from 1 to 255.");
        return percentage;
    }
    Amaterasu.parseFecOverride = parseFecOverride;
    function parseBackButtonTimeoutOverride(value) {
        const trimmed = value.trim();
        if (trimmed === "")
            return null;
        if (!/^-?[0-9]+$/.test(trimmed))
            throw new Error("Back button timeout must be a whole signed 32-bit number, or blank to inherit.");
        const timeout = Number(trimmed);
        if (!Number.isSafeInteger(timeout) || timeout < -0x8000_0000 || timeout > 0x7fff_ffff)
            throw new Error("Back button timeout must be a whole signed 32-bit number, or blank to inherit.");
        return timeout;
    }
    Amaterasu.parseBackButtonTimeoutOverride = parseBackButtonTimeoutOverride;
    function parseInputBooleanOverride(value, name) {
        if (value === "")
            return null;
        if (value === "true")
            return true;
        if (value === "false")
            return false;
        throw new Error(`${name} must be Inherit, Enabled, or Disabled.`);
    }
    Amaterasu.parseInputBooleanOverride = parseInputBooleanOverride;
    function parseGamepadProfileOverride(value, recognizedProfiles) {
        if (value === "")
            return null;
        if (typeof value !== "string" || !recognizedProfiles.includes(value))
            throw new Error("Choose a recognized gamepad profile for this host platform.");
        return value;
    }
    Amaterasu.parseGamepadProfileOverride = parseGamepadProfileOverride;
    function providerConfigOverrides(value, streamAudioValue = "", audioSinkMode = "inherit", audioSinkId = "", audioSinkCaptureOnlyValue = "", backButtonTimeoutValue = "", virtualSinkMode = "inherit", virtualSinkSelector = "", controllerValue = "", gamepadValue = "", keyboardValue = "", mouseValue = "", recognizedGamepadProfiles = ["auto"]) {
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
    Amaterasu.providerConfigOverrides = providerConfigOverrides;
    function parseAudioSinkOverride(mode, endpoint) {
        if (mode === "inherit")
            return null;
        if (mode === "automatic")
            return "";
        if (mode !== "specific" || endpoint.length === 0 ||
            new TextEncoder().encode(endpoint).length > 4096 ||
            /[\u0000-\u001f\u007f-\u009f]/.test(endpoint))
            throw new Error("Choose an audio endpoint ID of at most 4096 UTF-8 bytes without control characters.");
        return endpoint;
    }
    Amaterasu.parseAudioSinkOverride = parseAudioSinkOverride;
    function parseVirtualSinkOverride(mode, selector) {
        if (mode === "inherit")
            return null;
        if (mode === "automatic")
            return "";
        if (mode !== "specific" || selector.length === 0 ||
            new TextEncoder().encode(selector).length > 4096 ||
            /[\u0000-\u001f\u007f-\u009f]/.test(selector))
            throw new Error("Choose a virtual sink selector of at most 4096 UTF-8 bytes without control characters.");
        return selector;
    }
    Amaterasu.parseVirtualSinkOverride = parseVirtualSinkOverride;
    function parseStreamAudioOverride(value) {
        if (value === "")
            return null;
        if (value === "true")
            return true;
        if (value === "false")
            return false;
        throw new Error("Choose Inherit, Enabled, or Disabled for audio streaming.");
    }
    Amaterasu.parseStreamAudioOverride = parseStreamAudioOverride;
    function parseAudioSinkCaptureOnlyOverride(value) {
        if (value === "")
            return null;
        if (value === "true")
            return true;
        if (value === "false")
            return false;
        throw new Error("Choose Inherit, Capture only, or Route defaults for audio output.");
    }
    Amaterasu.parseAudioSinkCaptureOnlyOverride = parseAudioSinkCaptureOnlyOverride;
    function parseSteamId(value) {
        if (value === "")
            return null;
        if (!/^[1-9][0-9]*$/.test(value) || value.length > 10 || Number(value) > 0xffff_ffff)
            throw new Error("Steam app ID must be a canonical nonzero 32-bit decimal number.");
        return value;
    }
    Amaterasu.parseSteamId = parseSteamId;
    function parseSteamInstallDir(value) {
        if (value === "")
            return null;
        if (value.trim() !== value ||
            new TextEncoder().encode(value).length > 1024 ||
            /[\u0000-\u001f\u007f-\u009f]/.test(value))
            throw new Error("Steam install directory must be a printable absolute path of at most 1024 UTF-8 bytes.");
        const normalized = value.replace(/\\/g, "/");
        let components;
        if (normalized.startsWith("//")) {
            components = normalized.slice(2).split("/").filter(Boolean);
            if (components.length < 3)
                throw new Error("Steam UNC install directory needs a game subdirectory.");
        }
        else if (/^[A-Za-z]:\//.test(normalized)) {
            components = normalized.slice(3).split("/").filter(Boolean);
            if (components.length < 1)
                throw new Error("Steam install directory cannot be a drive root.");
        }
        else if (value.startsWith("/") && normalized.startsWith("/")) {
            components = normalized.slice(1).split("/").filter(Boolean);
            if (components.length < 1)
                throw new Error("Steam install directory cannot be a filesystem root.");
        }
        else {
            throw new Error("Steam install directory must be an absolute path.");
        }
        if (components.some((part) => part === "." || part === ".."))
            throw new Error("Steam install directory cannot contain dot segments.");
        return value;
    }
    Amaterasu.parseSteamInstallDir = parseSteamInstallDir;
    function parseDetachedCommands(value) {
        const commands = value
            .split(/\r?\n/)
            .map((command) => command.trim())
            .filter(Boolean);
        if (commands.length > 16 ||
            commands.some((command) => new TextEncoder().encode(command).length > 2048 ||
                /[\u0000-\u001f\u007f]/.test(command))) {
            throw new Error("Enter up to 16 detached commands, each at most 2048 bytes without control characters.");
        }
        return commands;
    }
    Amaterasu.parseDetachedCommands = parseDetachedCommands;
    function parseExitTimeout(value) {
        const trimmed = value.trim();
        if (trimmed === "")
            return null;
        if (!/^[0-9]+$/.test(trimmed))
            throw new Error("Exit timeout must be a whole number from 0 to 300 seconds.");
        const seconds = Number(trimmed);
        if (!Number.isSafeInteger(seconds) || seconds > 300)
            throw new Error("Exit timeout must be a whole number from 0 to 300 seconds.");
        return seconds;
    }
    Amaterasu.parseExitTimeout = parseExitTimeout;
    function parsePreparationSteps(value) {
        let parsed;
        try {
            parsed = JSON.parse(value || "[]");
        }
        catch {
            throw new Error("Preparation commands must be a valid JSON array.");
        }
        if (!Array.isArray(parsed) || parsed.length > 8)
            throw new Error("Preparation commands must be a JSON array with at most 8 steps.");
        for (const step of parsed) {
            if (step === null ||
                Array.isArray(step) ||
                typeof step !== "object" ||
                !("run" in step) ||
                Object.keys(step).some((key) => !["run", "undo"].includes(key)))
                throw new Error("Each preparation step must contain run and an optional undo command.");
            validatePreparationCommand(step.run);
            if (step.undo != null)
                validatePreparationCommand(step.undo);
        }
        return parsed;
    }
    Amaterasu.parsePreparationSteps = parsePreparationSteps;
    function validatePreparationCommand(value) {
        if (value === null ||
            Array.isArray(value) ||
            typeof value !== "object" ||
            !("executable" in value) ||
            Object.keys(value).some((key) => !["executable", "args", "environment", "cwd", "timeout_ms"].includes(key)))
            throw new Error("Each preparation command needs an executable and only supported fields.");
        const command = value;
        if (typeof command.executable !== "string" ||
            !isAbsolutePreparationPath(command.executable) ||
            new TextEncoder().encode(command.executable).length > 1024 ||
            command.executable.includes("\0") ||
            /\.(bat|cmd)$/i.test(command.executable))
            throw new Error("Preparation executable must be a native absolute path of at most 1024 bytes; batch files need an explicit interpreter.");
        if (command.cwd != null &&
            (typeof command.cwd !== "string" ||
                !isAbsolutePreparationPath(command.cwd) ||
                new TextEncoder().encode(command.cwd).length > 1024 ||
                command.cwd.includes("\0")))
            throw new Error("Preparation working directory must be a native absolute path of at most 1024 bytes.");
        if (command.args !== undefined) {
            if (!Array.isArray(command.args) ||
                command.args.length > 64 ||
                command.args.some((argument) => typeof argument !== "string" ||
                    new TextEncoder().encode(argument).length > 1024 ||
                    argument.includes("\0")) ||
                command.args.reduce((bytes, argument) => bytes + (typeof argument === "string" ? new TextEncoder().encode(argument).length : 0), 0) > 8192)
                throw new Error("Preparation arguments exceed the 64 item and 8192 byte limits.");
        }
        if (command.timeout_ms !== undefined && (!Number.isSafeInteger(command.timeout_ms) ||
            command.timeout_ms < 1 ||
            command.timeout_ms > 30000))
            throw new Error("Preparation timeout must be a whole number from 1 to 30000 milliseconds.");
        if (command.environment !== undefined) {
            const environment = command.environment;
            if (environment === null || Array.isArray(environment) || typeof environment !== "object")
                throw new Error("Preparation environment must be a JSON object.");
            const entries = Object.entries(environment);
            const foldedNames = new Set();
            let totalBytes = 0;
            for (const [key, rawValue] of entries) {
                if (key.length === 0 ||
                    new TextEncoder().encode(key).length > 128 ||
                    !/^[A-Za-z0-9_]+$/.test(key) ||
                    typeof rawValue !== "string" ||
                    new TextEncoder().encode(rawValue).length > 1024 ||
                    rawValue.includes("\0") ||
                    foldedNames.has(key.toUpperCase()))
                    throw new Error("Preparation environment names and values exceed their limits or are invalid.");
                foldedNames.add(key.toUpperCase());
                totalBytes += new TextEncoder().encode(key).length + new TextEncoder().encode(rawValue).length;
            }
            if (entries.length > 32 || totalBytes > 4096)
                throw new Error("Preparation environment exceeds the 32 variable and 4096 byte limits.");
        }
    }
    function isAbsolutePreparationPath(value) {
        return value.startsWith("/") ||
            /^[A-Za-z]:[\\/]/.test(value) ||
            /^\\\\[^\\/]+[\\/][^\\/]+/.test(value);
    }
    function parsePreparation(value) {
        try {
            return parsePreparationSteps(value);
        }
        catch {
            throw new Error("Existing preparation commands are malformed or outside supported bounds; reload before saving.");
        }
    }
    function entryFromForm(data, recognizedGamepadProfiles = ["auto"]) {
        const text = (name) => String(data.get(name) ?? "").trim();
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
        const refreshRateOption = parseRefreshRateOptionOverride(String(data.get("dd_refresh_rate_option") ?? ""));
        const prefer10BitSdr = parsePrefer10BitSdrOverride(String(data.get("prefer_10bit_sdr") ?? ""));
        const steamId = kind === "playnite_fullscreen" ? null : parseSteamId(String(data.get("steam_id") ?? ""));
        const steamInstallDir = kind === "playnite_fullscreen" ? null : parseSteamInstallDir(String(data.get("steam_install_dir") ?? ""));
        const configOverrides = fecPercentage === null &&
            streamAudio === null &&
            audioSink === null &&
            virtualSink === null &&
            audioSinkCaptureOnly === null &&
            backButtonTimeout === null &&
            controller === null &&
            gamepad === null &&
            keyboard === null &&
            mouse === null &&
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
                ...(refreshRateOption === null ? {} : { dd_refresh_rate_option: refreshRateOption }),
                ...(prefer10BitSdr === null ? {} : { prefer_10bit_sdr: prefer10BitSdr }),
            };
        const exitTimeoutSeconds = kind === "executable" ? parseExitTimeout(text("exit_timeout_seconds")) : null;
        const detached = kind === "executable" ? parseDetachedCommands(text("detached")) : [];
        const preparation = kind === "executable" ? parsePreparation(text("preparation")) : [];
        const launch = kind === "playnite_fullscreen"
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
            launch,
            ...(steamId === null ? {} : { steam_id: steamId }),
            ...(steamInstallDir === null ? {} : { steam_install_dir: steamInstallDir }),
            ...(configOverrides ? { config_overrides: configOverrides } : {}),
        };
    }
    Amaterasu.entryFromForm = entryFromForm;
    function fieldValue(field, input) {
        if (field.type === "boolean")
            return input.checked;
        if (field.type === "string")
            return input.value;
        if (field.type === "enum") {
            if (!field.options.includes(input.value))
                throw new Error("Choose a listed setting value.");
            return input.value;
        }
        if (field.type === "list" || field.type === "object") {
            let value;
            try {
                value = JSON.parse(input.value);
            }
            catch {
                throw new Error("Enter valid JSON for this setting.");
            }
            const wrongShape = field.type === "list"
                ? !Array.isArray(value)
                : value === null || Array.isArray(value) || typeof value !== "object";
            if (wrongShape)
                throw new Error(`Enter a JSON ${field.type === "list" ? "array" : "object"} for this setting.`);
            return value;
        }
        if (!input.value.trim())
            throw new Error("Numeric settings cannot be empty.");
        const value = Number(input.value);
        if (!Number.isFinite(value) ||
            (field.type === "integer" && !Number.isSafeInteger(value)))
            throw new Error(field.type === "integer"
                ? "Enter a whole number within the safe numeric range."
                : "Enter a finite decimal number.");
        if ((field.minimum != null && value < field.minimum) ||
            (field.maximum != null && value > field.maximum))
            throw new Error("Setting value is outside its allowed range.");
        return value;
    }
    Amaterasu.fieldValue = fieldValue;
    function launchPath(entry) {
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
    Amaterasu.launchPath = launchPath;
    function countLabel(count, more) {
        return `${count}${more != null ? "+" : ""}`;
    }
    Amaterasu.countLabel = countLabel;
    function messageOf(error) {
        return error instanceof Error
            ? error.message
            : "The request could not be completed.";
    }
    Amaterasu.messageOf = messageOf;
})(Amaterasu || (Amaterasu = {}));
var Amaterasu;
(function (Amaterasu) {
    const SESSION_STORAGE_VERSION = 1;
    class ApiError extends Error {
        code;
        status;
        constructor(message, code, status) {
            super(message);
            this.code = code;
            this.status = status;
        }
    }
    Amaterasu.ApiError = ApiError;
    class Api {
        bearer = "";
        refreshToken = "";
        accessExpiresAt = 0;
        authGeneration = 0;
        persistenceGeneration = 0;
        tokenGeneration = 0;
        refreshInFlight = null;
        logoutInFlight = null;
        requests = new Set();
        refreshRequests = new Set();
        base;
        storageKey;
        refreshLockName;
        cookieLogoutKey;
        cookiePendingLogoutKey;
        usesSecureCookies;
        cookieSession = false;
        cookieRefreshInFlight = null;
        remembered = false;
        sessionId = null;
        loggingOut = false;
        onAuthenticationLost = null;
        subject = "";
        scopes = null;
        constructor(base) {
            const url = new URL(base || location.origin, location.origin);
            if (!["http:", "https:"].includes(url.protocol) ||
                url.username ||
                url.password ||
                url.search ||
                url.hash)
                throw new Error("The API address must be an HTTP(S) origin.");
            this.base = url.origin;
            this.usesSecureCookies =
                url.protocol === "https:" && this.base === location.origin;
            this.storageKey = `amaterasu.session.${encodeURIComponent(this.base)}`;
            this.refreshLockName = `amaterasu.session.refresh:${this.base}`;
            this.cookieLogoutKey = `amaterasu.session.cookie-logout.${encodeURIComponent(this.base)}`;
            this.cookiePendingLogoutKey = `amaterasu.session.cookie-pending-logout.${encodeURIComponent(this.base)}`;
            if (typeof window !== "undefined" &&
                typeof window.addEventListener === "function") {
                window.addEventListener("storage", (event) => this.handleStorageChange(event));
            }
        }
        async login(username, password, rememberMe = false) {
            if (this.loggingOut) {
                throw new ApiError("Sign out is already in progress.", "unauthorized", 401);
            }
            if (this.usesSecureCookies) {
                await this.loginWithCookies(username, password, rememberMe);
                return;
            }
            if (rememberMe && !this.webLocks())
                throw this.rememberedRefreshLockError();
            const generation = this.authGeneration;
            const reply = await this.post("auth/login", { username, password, remember_me: rememberMe }, false);
            await this.installSession(reply, generation, true, rememberMe);
        }
        async restoreSession() {
            if (this.loggingOut)
                return false;
            if (this.usesSecureCookies)
                return this.restoreCookieSession();
            this.resetMemory(true);
            if (!this.webLocks()) {
                return false;
            }
            const generation = this.authGeneration;
            const persistenceGeneration = this.persistenceGeneration;
            const stored = await this.withSessionLock(() => {
                if (generation !== this.authGeneration ||
                    persistenceGeneration !== this.persistenceGeneration)
                    return null;
                const candidate = this.readStoredSession();
                if (candidate &&
                    generation === this.authGeneration &&
                    persistenceGeneration === this.persistenceGeneration)
                    this.installStoredSession(candidate);
                return candidate;
            });
            if (!stored)
                return false;
            const token = this.refreshToken;
            if (stored.expires_at - Date.now() / 1000 <= 60)
                await this.refreshOrClear(generation, token, persistenceGeneration);
            return generation === this.authGeneration && this.bearer !== "";
        }
        suspend() {
            this.resetMemory(false);
        }
        async useToken(token) {
            if (this.loggingOut) {
                throw new ApiError("Sign out is already in progress.", "unauthorized", 401);
            }
            if (!/^ama_[a-f0-9]{64}$/.test(token))
                throw new Error("Enter an Amaterasu token beginning with ama_ followed by 64 hexadecimal characters.");
            await this.clear();
            this.bearer = token;
            this.subject = "API token";
            this.scopes = null;
            this.tokenGeneration += 1;
        }
        async logout() {
            if (this.logoutInFlight)
                return this.logoutInFlight;
            if (this.cookieSession)
                return this.logoutCookieSession();
            this.loggingOut = true;
            this.subject = "";
            this.scopes = null;
            this.abortRequests(true);
            const pendingLogout = this.finishLogout();
            this.logoutInFlight = pendingLogout;
            try {
                await pendingLogout;
            }
            finally {
                if (this.logoutInFlight === pendingLogout)
                    this.logoutInFlight = null;
                this.loggingOut = false;
            }
        }
        async finishLogout() {
            const sessionId = this.remembered ? this.sessionId : null;
            const refreshTokens = new Set();
            const originalRefreshToken = this.refreshToken;
            const originalBearerToken = originalRefreshToken ? "" : this.bearer;
            if (originalRefreshToken)
                refreshTokens.add(originalRefreshToken);
            const pendingRefresh = this.refreshInFlight;
            if (pendingRefresh)
                await pendingRefresh.catch(() => undefined);
            const sameSession = sessionId === null
                ? !this.remembered
                : this.remembered && this.sessionId === sessionId;
            if (sameSession && this.refreshToken)
                refreshTokens.add(this.refreshToken);
            const bearerToken = sameSession && !this.refreshToken ? this.bearer : originalBearerToken;
            this.resetMemory(true);
            const locks = this.webLocks();
            if (locks) {
                try {
                    await locks.request(this.refreshLockName, { mode: "exclusive" }, () => {
                        const latest = this.readStoredSessionForCleanup(new Set(), sessionId);
                        if (latest) {
                            refreshTokens.add(latest.refresh_token);
                            this.removeStoredSessionBestEffort(sessionId);
                        }
                    });
                }
                catch {
                }
            }
            else {
                this.removeStoredSessionBestEffort(sessionId);
            }
            for (const refreshToken of refreshTokens) {
                await this.revokeSession({ refresh_token: refreshToken });
            }
            if (bearerToken)
                await this.revokeSession({ bearer: bearerToken });
        }
        async clear() {
            if (this.cookieSession) {
                await this.logoutCookieSession();
                return;
            }
            const sessionId = this.remembered ? this.sessionId : null;
            this.resetMemory(true);
            await this.removeStoredSessionForIdentity(sessionId);
        }
        allows(scope) {
            return this.scopes === null || this.scopes.includes(scope);
        }
        async post(route, payload = {}, protectedRoute = true) {
            if (this.loggingOut) {
                throw new ApiError("Sign out is already in progress.", "unauthorized", 401);
            }
            if (protectedRoute && this.cookieSession)
                return this.postWithCookies(route, payload);
            if (protectedRoute && !this.bearer)
                throw new ApiError("Sign in to continue.", "unauthorized", 401);
            if (protectedRoute &&
                this.refreshToken &&
                this.accessExpiresAt - Date.now() / 1000 <= 60) {
                await this.refreshOrClear();
            }
            if (protectedRoute && !this.bearer)
                throw new ApiError("Sign in to continue.", "unauthorized", 401);
            const bearerUsed = this.bearer;
            const authGeneration = this.authGeneration;
            const tokenGeneration = this.tokenGeneration;
            try {
                const response = await this.send(route, payload, protectedRoute, bearerUsed);
                if (this.loggingOut) {
                    throw new ApiError("Sign out is already in progress.", "unauthorized", 401);
                }
                return response;
            }
            catch (error) {
                if (!protectedRoute ||
                    !(error instanceof ApiError) ||
                    error.code !== "unauthorized" ||
                    this.loggingOut ||
                    !this.refreshToken ||
                    authGeneration !== this.authGeneration) {
                    throw error;
                }
                if (tokenGeneration === this.tokenGeneration)
                    await this.refreshOrClear();
                if (authGeneration !== this.authGeneration || !this.bearer)
                    throw error;
                const response = await this.send(route, payload, true, this.bearer);
                if (this.loggingOut) {
                    throw new ApiError("Sign out is already in progress.", "unauthorized", 401);
                }
                return response;
            }
        }
        validateBrowserSession(reply) {
            if (reply.ok !== true ||
                typeof reply.subject !== "string" ||
                !Array.isArray(reply.scopes) ||
                !reply.scopes.every((scope) => typeof scope === "string") ||
                !Number.isSafeInteger(reply.expires_at) ||
                reply.expires_at <= 0 ||
                Object.hasOwn(reply, "bearer") ||
                Object.hasOwn(reply, "refresh_token"))
                throw new ApiError("The host returned an invalid authentication response.", "invalid_response", 502);
            return reply;
        }
        applyBrowserSession(reply) {
            const session = this.validateBrowserSession(reply);
            this.cookieSession = true;
            this.accessExpiresAt = session.expires_at;
            this.subject = session.subject;
            this.scopes = [...session.scopes];
            this.tokenGeneration += 1;
        }
        async loginWithCookies(username, password, rememberMe) {
            this.assertCookieStorageAvailable();
            if (!this.webLocks())
                throw this.rememberedRefreshLockError();
            let generation = this.authGeneration;
            await this.withSessionLock(async () => {
                if (await this.finishPendingCookieLogout())
                    throw new Error("The sign-in attempt was cancelled by a pending sign-out.");
                if (generation !== this.authGeneration)
                    throw new Error("The sign-in attempt was cancelled.");
                await this.completeCookieLogout(this.markCookieLogoutPending());
                this.resetMemory(true);
                generation = this.authGeneration;
                const reply = await this.send("auth/browser/login", { username, password, remember_me: rememberMe }, false, "", true);
                if (this.pendingCookieLogout()) {
                    await this.finishPendingCookieLogout();
                    throw new Error("The sign-in attempt was cancelled by a pending sign-out.");
                }
                if (generation !== this.authGeneration) {
                    await this.completeCookieLogout(this.markCookieLogoutPending());
                    throw new Error("The sign-in attempt was cancelled.");
                }
                try {
                    this.applyBrowserSession(reply);
                }
                catch (error) {
                    await this.completeCookieLogout(this.markCookieLogoutPending());
                    throw error;
                }
                this.authGeneration += 1;
            });
        }
        async restoreCookieSession() {
            this.resetMemory(true);
            this.assertCookieStorageAvailable();
            if (!this.webLocks())
                return false;
            const generation = this.authGeneration;
            return this.withSessionLock(async () => {
                if (await this.finishPendingCookieLogout())
                    return false;
                try {
                    const reply = await this.readCookieSession();
                    if (generation !== this.authGeneration || this.loggingOut)
                        return false;
                    this.applyBrowserSession(reply);
                    return true;
                }
                catch (error) {
                    if (error instanceof ApiError && error.status === 401)
                        return false;
                    throw error;
                }
            });
        }
        async refreshCookieSession() {
            if (this.cookieRefreshInFlight)
                return this.cookieRefreshInFlight;
            const generation = this.authGeneration;
            const pending = this.withSessionLock(async () => {
                if (generation !== this.authGeneration || this.loggingOut)
                    return;
                const reply = await this.readCookieSession();
                if (generation === this.authGeneration && !this.loggingOut)
                    this.applyBrowserSession(reply);
            });
            this.cookieRefreshInFlight = pending;
            try {
                await pending;
            }
            catch (error) {
                if (error instanceof ApiError &&
                    (error.status === 401 || error.code === "logout_unconfirmed") &&
                    generation === this.authGeneration && !this.loggingOut) {
                    this.resetMemory(true);
                    this.onAuthenticationLost?.();
                }
                throw error;
            }
            finally {
                if (this.cookieRefreshInFlight === pending)
                    this.cookieRefreshInFlight = null;
            }
        }
        async postWithCookies(route, payload) {
            if (this.pendingCookieLogout())
                throw new ApiError("Sign-out is still pending. Retry when the host is reachable.", "logout_unconfirmed", 503);
            if (this.accessExpiresAt - Date.now() / 1000 <= 60)
                await this.refreshCookieSession();
            if (!this.cookieSession || this.loggingOut)
                throw new ApiError("Sign in to continue.", "unauthorized", 401);
            const generation = this.authGeneration;
            const tokenGeneration = this.tokenGeneration;
            try {
                const reply = await this.send(route, payload, true, "", true);
                if (generation !== this.authGeneration || this.loggingOut)
                    throw new ApiError("Sign in to continue.", "unauthorized", 401);
                return reply;
            }
            catch (error) {
                if (!(error instanceof ApiError) || error.status !== 401 ||
                    generation !== this.authGeneration || this.loggingOut)
                    throw error;
                if (tokenGeneration === this.tokenGeneration)
                    await this.refreshCookieSession();
                if (!this.cookieSession || generation !== this.authGeneration)
                    throw error;
                try {
                    const reply = await this.send(route, payload, true, "", true);
                    if (generation !== this.authGeneration || this.loggingOut)
                        throw new ApiError("Sign in to continue.", "unauthorized", 401);
                    return reply;
                }
                catch (retryError) {
                    if (retryError instanceof ApiError &&
                        retryError.status === 401 &&
                        generation === this.authGeneration) {
                        this.resetMemory(true);
                        this.onAuthenticationLost?.();
                    }
                    throw retryError;
                }
            }
        }
        async requestCookieRefresh() {
            try {
                return this.validateBrowserSession(await this.send("auth/browser/refresh", {}, false, "", true));
            }
            catch (error) {
                if (error instanceof ApiError && error.status === 401)
                    await this.completeCookieLogout(this.markCookieLogoutPending());
                throw error;
            }
        }
        async readCookieSession() {
            let reply;
            try {
                reply = this.validateBrowserSession(await this.send("auth/browser/session", {}, false, "", true));
            }
            catch (error) {
                if (!(error instanceof ApiError) || error.status !== 401)
                    throw error;
                return this.requestCookieRefresh();
            }
            if (reply.expires_at - Date.now() / 1000 <= 60)
                return this.requestCookieRefresh();
            return reply;
        }
        async logoutCookieSession() {
            if (this.logoutInFlight)
                return this.logoutInFlight;
            const marker = this.markCookieLogoutPending();
            this.loggingOut = true;
            this.resetMemory(true, true);
            const pending = this.withSessionLock(async () => {
                if (this.readPendingCookieLogout() !== marker)
                    throw new ApiError("Sign-out could not be confirmed because another browser session changed while it was waiting. Review the current session before retrying.", "logout_unconfirmed", 503);
                await this.completeCookieLogout(marker);
            });
            this.logoutInFlight = pending;
            try {
                await pending;
            }
            finally {
                this.logoutInFlight = null;
                this.loggingOut = false;
            }
        }
        markCookieLogoutPending() {
            if (this.pendingCookieLogout())
                throw new ApiError("Another sign-out is pending. Retry after it completes.", "logout_unconfirmed", 503);
            const marker = `${Date.now()}:${crypto.randomUUID()}`;
            try {
                localStorage.setItem(this.cookiePendingLogoutKey, marker);
                if (localStorage.getItem(this.cookiePendingLogoutKey) !== marker)
                    throw new Error("Browser storage did not retain the logout marker.");
            }
            catch {
                throw this.cookieStorageError();
            }
            return marker;
        }
        pendingCookieLogout() {
            return this.readPendingCookieLogout() !== null;
        }
        readPendingCookieLogout() {
            try {
                return localStorage.getItem(this.cookiePendingLogoutKey);
            }
            catch {
                throw this.cookieStorageError();
            }
        }
        async finishPendingCookieLogout() {
            const marker = this.readPendingCookieLogout();
            if (marker === null)
                return false;
            await this.completeCookieLogout(marker);
            return true;
        }
        assertCookieStorageAvailable() {
            const probeKey = `${this.cookiePendingLogoutKey}.probe`;
            const probe = crypto.randomUUID();
            try {
                localStorage.setItem(probeKey, probe);
                if (localStorage.getItem(probeKey) !== probe)
                    throw new Error("Browser storage did not retain the logout marker.");
                localStorage.removeItem(probeKey);
            }
            catch {
                throw this.cookieStorageError();
            }
        }
        cookieStorageError() {
            return new ApiError("Secure browser sign-in requires writable storage to recover an unconfirmed sign-out after reload.", "storage_unavailable", 503);
        }
        async completeCookieLogout(marker) {
            try {
                await this.send("auth/browser/logout", {}, false, "", true);
            }
            catch (error) {
                if (!(error instanceof ApiError) || error.status !== 401 ||
                    error.code !== "session_missing")
                    throw new ApiError("Could not confirm sign-out. The browser may still hold session cookies; retry when the host is reachable.", "logout_unconfirmed", 503);
            }
            try {
                if (localStorage.getItem(this.cookiePendingLogoutKey) === marker) {
                    localStorage.removeItem(this.cookiePendingLogoutKey);
                    if (localStorage.getItem(this.cookiePendingLogoutKey) === marker)
                        throw new Error("Browser storage retained the logout marker.");
                }
            }
            catch {
                throw this.cookieStorageError();
            }
            try {
                localStorage.setItem(this.cookieLogoutKey, `${Date.now()}:${crypto.randomUUID()}`);
            }
            catch {
            }
        }
        async refreshOrClear(expectedGeneration = this.authGeneration, expectedToken = this.refreshToken, expectedPersistenceGeneration = this.persistenceGeneration) {
            try {
                await this.refreshSession();
            }
            catch (error) {
                if (!this.loggingOut &&
                    this.refreshToken &&
                    expectedGeneration === this.authGeneration &&
                    expectedToken === this.refreshToken &&
                    expectedPersistenceGeneration === this.persistenceGeneration) {
                    await this.clear();
                    this.onAuthenticationLost?.();
                }
                throw error;
            }
        }
        async refreshSession() {
            if (this.refreshInFlight)
                return this.refreshInFlight;
            if (!this.refreshToken)
                throw new ApiError("Sign in again to continue.", "unauthorized", 401);
            const generation = this.authGeneration;
            const persistenceGeneration = this.persistenceGeneration;
            const token = this.refreshToken;
            const suspendGeneration = this.authGeneration;
            const pending = this.remembered
                ? this.refreshRememberedSession(generation, persistenceGeneration, suspendGeneration, token)
                : this.refreshMemorySession(generation, suspendGeneration, token);
            this.refreshInFlight = pending;
            try {
                await pending;
            }
            finally {
                if (this.refreshInFlight === pending)
                    this.refreshInFlight = null;
            }
        }
        async refreshMemorySession(generation, suspendGeneration, token) {
            const reply = await this.send("auth/refresh", { refresh_token: token }, false, "");
            const validated = this.validateSession(reply);
            if (generation !== this.authGeneration || token !== this.refreshToken) {
                void this.revokeSession({ refresh_token: validated.refresh_token });
                return;
            }
            if (suspendGeneration !== this.authGeneration)
                return;
            this.applyValidatedSession(validated, false);
        }
        async refreshRememberedSession(generation, persistenceGeneration, suspendGeneration, token) {
            const locks = this.webLocks();
            if (!locks)
                throw this.rememberedRefreshLockError();
            await locks.request(this.refreshLockName, { mode: "exclusive" }, async () => {
                if (generation !== this.authGeneration ||
                    persistenceGeneration !== this.persistenceGeneration)
                    return;
                const stored = this.readStoredSession();
                if (!stored) {
                    throw new ApiError("Sign in again to continue.", "unauthorized", 401);
                }
                if (stored.refresh_token !== token ||
                    stored.bearer !== this.bearer) {
                    this.installStoredSession(stored);
                    return;
                }
                const refreshingRecord = {
                    ...stored,
                    refreshing: true,
                };
                this.writeStoredSession(refreshingRecord);
                let reply;
                try {
                    reply = await this.send("auth/refresh", { refresh_token: token }, false, "");
                }
                catch (error) {
                    throw error;
                }
                const validated = this.validateSession(reply);
                const currentRaw = this.readStoredSessionRaw();
                const expectedRaw = JSON.stringify(refreshingRecord);
                if (currentRaw !== expectedRaw ||
                    persistenceGeneration !== this.persistenceGeneration) {
                    void this.revokeSession({ refresh_token: validated.refresh_token });
                    const latest = currentRaw
                        ? this.parseStoredSession(currentRaw, false)
                        : null;
                    if (latest &&
                        generation === this.authGeneration &&
                        persistenceGeneration === this.persistenceGeneration) {
                        this.installStoredSession(latest);
                        return;
                    }
                    if (generation === this.authGeneration) {
                        throw new ApiError("Sign in again to continue.", "unauthorized", 401);
                    }
                    return;
                }
                const rotatedRecord = this.storedSession(validated, stored.session_id);
                this.writeStoredSession(rotatedRecord);
                if (generation === this.authGeneration &&
                    suspendGeneration === this.authGeneration &&
                    persistenceGeneration === this.persistenceGeneration) {
                    this.applyValidatedSession(validated, true, stored.session_id);
                }
            });
        }
        async installSession(reply, generation, newSession, rememberRequested) {
            if (generation !== this.authGeneration)
                throw new Error("The sign-in attempt was cancelled.");
            const validated = this.validateSession(reply);
            const shouldRemember = rememberRequested && validated.remember_me;
            const previousSessionId = this.remembered ? this.sessionId : null;
            let sessionId = null;
            if (newSession) {
                try {
                    if (shouldRemember) {
                        const locks = this.webLocks();
                        if (!locks)
                            throw this.rememberedRefreshLockError();
                        const stored = this.storedSession(validated);
                        await locks.request(this.refreshLockName, { mode: "exclusive" }, () => {
                            if (generation !== this.authGeneration)
                                throw new Error("The sign-in attempt was cancelled.");
                            this.writeStoredSession(stored);
                        });
                        sessionId = stored.session_id;
                    }
                    else {
                        await this.removeStoredSessionForIdentity(previousSessionId);
                    }
                }
                catch (error) {
                    if (generation === this.authGeneration &&
                        (this.remembered ? this.sessionId : null) === previousSessionId)
                        await this.clear();
                    if (shouldRemember)
                        void this.revokeSession({ refresh_token: validated.refresh_token });
                    throw error;
                }
            }
            if (generation !== this.authGeneration)
                throw new Error("The sign-in attempt was cancelled.");
            this.applyValidatedSession(validated, shouldRemember, sessionId);
            if (newSession) {
                this.authGeneration += 1;
                this.persistenceGeneration += 1;
            }
        }
        validateSession(reply) {
            if (!/^ama_[a-f0-9]{64}$/.test(reply.bearer) ||
                !/^ama_[a-f0-9]{64}$/.test(reply.refresh_token) ||
                typeof reply.subject !== "string" ||
                !Array.isArray(reply.scopes) ||
                !reply.scopes.every((scope) => typeof scope === "string") ||
                !Number.isSafeInteger(reply.expires_at) ||
                !Number.isSafeInteger(reply.refresh_expires_at) ||
                reply.expires_at <= 0 ||
                reply.refresh_expires_at < reply.expires_at ||
                typeof reply.remember_me !== "boolean") {
                throw new ApiError("The host returned an invalid authentication response.", "invalid_response", 502);
            }
            return {
                bearer: reply.bearer,
                refresh_token: reply.refresh_token,
                subject: reply.subject,
                scopes: [...reply.scopes],
                expires_at: reply.expires_at,
                refresh_expires_at: reply.refresh_expires_at,
                remember_me: reply.remember_me,
            };
        }
        applyValidatedSession(session, remembered, sessionId = null) {
            this.bearer = session.bearer;
            this.refreshToken = session.refresh_token;
            this.accessExpiresAt = session.expires_at;
            this.subject = this.loggingOut ? "" : session.subject;
            this.scopes = this.loggingOut ? null : [...session.scopes];
            this.remembered = remembered;
            this.sessionId = remembered ? sessionId : null;
            this.tokenGeneration += 1;
        }
        installStoredSession(session) {
            this.applyValidatedSession(session, true, session.session_id);
        }
        storedSession(session, sessionId = this.newSessionId()) {
            return {
                ...session,
                remember_me: true,
                version: SESSION_STORAGE_VERSION,
                base: this.base,
                session_id: sessionId,
                refreshing: false,
            };
        }
        newSessionId() {
            const bytes = crypto.getRandomValues(new Uint8Array(16));
            bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x40;
            bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
            const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
            return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
        }
        readStoredSession() {
            const raw = this.readStoredSessionRaw();
            if (raw === null)
                return null;
            const stored = this.parseStoredSession(raw, false);
            if (stored)
                return stored;
            this.removeStoredSessionRequired(null, raw);
            return null;
        }
        readStoredSessionRaw() {
            const storage = this.storage(false);
            if (!storage)
                return null;
            try {
                return storage.getItem(this.storageKey);
            }
            catch {
                throw this.storageError();
            }
        }
        parseStoredSession(raw, allowRefreshing) {
            let value;
            try {
                value = JSON.parse(raw);
            }
            catch {
                return null;
            }
            if (!value || typeof value !== "object")
                return null;
            const record = value;
            if (record.version !== SESSION_STORAGE_VERSION ||
                record.base !== this.base ||
                typeof record.session_id !== "string" ||
                !/^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/i.test(record.session_id) ||
                record.remember_me !== true ||
                (record.refreshing !== false &&
                    !(allowRefreshing && record.refreshing === true)) ||
                typeof record.bearer !== "string" ||
                !/^ama_[a-f0-9]{64}$/.test(record.bearer) ||
                typeof record.refresh_token !== "string" ||
                !/^ama_[a-f0-9]{64}$/.test(record.refresh_token) ||
                typeof record.subject !== "string" ||
                !Array.isArray(record.scopes) ||
                !record.scopes.every((scope) => typeof scope === "string") ||
                !Number.isSafeInteger(record.expires_at) ||
                !Number.isSafeInteger(record.refresh_expires_at) ||
                record.expires_at <= 0 ||
                record.refresh_expires_at <
                    record.expires_at ||
                record.refresh_expires_at <= Date.now() / 1000) {
                return null;
            }
            return record;
        }
        writeStoredSession(session) {
            const storage = this.storage(true);
            if (!storage)
                throw this.storageError();
            try {
                storage.setItem(this.storageKey, JSON.stringify(session));
            }
            catch {
                throw this.storageError();
            }
        }
        removeStoredSessionRequired(expectedSessionId, expectedRaw) {
            const storage = this.storage(false);
            if (!storage)
                return;
            try {
                if (expectedSessionId !== undefined) {
                    const currentRaw = storage.getItem(this.storageKey);
                    if (currentRaw === null ||
                        !this.canRemoveStoredSession(currentRaw, expectedSessionId, expectedRaw))
                        return;
                }
                storage.removeItem(this.storageKey);
            }
            catch {
                try {
                    if (expectedSessionId !== undefined) {
                        const currentRaw = storage.getItem(this.storageKey);
                        if (currentRaw === null ||
                            !this.canRemoveStoredSession(currentRaw, expectedSessionId, expectedRaw))
                            return;
                    }
                    storage.setItem(this.storageKey, "{}");
                }
                catch {
                    throw this.storageError();
                }
            }
        }
        removeStoredSessionBestEffort(expectedSessionId, expectedRaw) {
            let storage;
            try {
                storage = this.storage(false);
            }
            catch {
                return;
            }
            if (!storage)
                return;
            try {
                if (expectedSessionId !== undefined) {
                    const currentRaw = storage.getItem(this.storageKey);
                    if (currentRaw === null ||
                        !this.canRemoveStoredSession(currentRaw, expectedSessionId, expectedRaw))
                        return;
                }
                storage.removeItem(this.storageKey);
            }
            catch {
                try {
                    if (expectedSessionId !== undefined) {
                        const currentRaw = storage.getItem(this.storageKey);
                        if (currentRaw === null ||
                            !this.canRemoveStoredSession(currentRaw, expectedSessionId, expectedRaw))
                            return;
                    }
                    storage.setItem(this.storageKey, "{}");
                }
                catch {
                }
            }
        }
        async removeStoredSessionForIdentity(expectedSessionId, expectedRaw) {
            const removeCurrentSession = () => {
                try {
                    const raw = this.readStoredSessionRaw();
                    if (raw === null || (expectedRaw !== undefined && raw !== expectedRaw))
                        return;
                    const current = this.parseStoredSession(raw, true);
                    if (current &&
                        (expectedRaw !== undefined ||
                            expectedSessionId === null ||
                            current.session_id !== expectedSessionId))
                        return;
                    this.removeStoredSessionRequired(expectedSessionId, raw);
                }
                catch {
                }
            };
            try {
                await this.withSessionLock(removeCurrentSession);
            }
            catch {
                if (!this.webLocks())
                    removeCurrentSession();
            }
        }
        readStoredSessionForCleanup(tokens, expectedSessionId) {
            try {
                const raw = this.readStoredSessionRaw();
                if (raw === null)
                    return null;
                const stored = this.parseStoredSession(raw, true);
                if (stored &&
                    expectedSessionId !== null &&
                    stored.session_id === expectedSessionId) {
                    tokens.add(stored.refresh_token);
                    return stored;
                }
            }
            catch {
            }
            return null;
        }
        canRemoveStoredSession(raw, expectedSessionId, expectedRaw) {
            if (expectedRaw !== undefined)
                return raw === expectedRaw;
            if (expectedSessionId === undefined)
                return true;
            const stored = this.parseStoredSession(raw, true);
            if (!stored)
                return true;
            return (expectedSessionId !== null &&
                stored.base === this.base &&
                stored.session_id === expectedSessionId);
        }
        async revokeSession(payload) {
            try {
                await this.send("auth/logout", payload, false, "");
            }
            catch {
            }
        }
        webLocks() {
            if (typeof navigator === "undefined")
                return null;
            try {
                return navigator.locks ?? null;
            }
            catch {
                return null;
            }
        }
        async withSessionLock(action) {
            const locks = this.webLocks();
            if (!locks)
                return action();
            return locks.request(this.refreshLockName, { mode: "exclusive" }, action);
        }
        rememberedRefreshLockError() {
            return new ApiError("Remember me requires Web Locks in a secure browser context. Sign in without Remember me when unavailable.", "refresh_lock_unavailable", 503);
        }
        storage(required) {
            if (typeof localStorage === "undefined") {
                if (required)
                    throw this.storageError();
                return null;
            }
            try {
                return localStorage;
            }
            catch {
                throw this.storageAccessError();
            }
        }
        storageError() {
            return new ApiError("Browser storage is unavailable. Sign in again when storage is enabled.", "storage_unavailable", 503);
        }
        storageAccessError() {
            return new ApiError("Browser storage is unavailable. Sign in again when storage is enabled.", "storage_access_unavailable", 503);
        }
        handleStorageChange(event) {
            if (this.loggingOut)
                return;
            if (event.key === this.cookiePendingLogoutKey &&
                event.newValue !== null && this.cookieSession) {
                let stillPending = true;
                try {
                    stillPending =
                        localStorage.getItem(this.cookiePendingLogoutKey) === event.newValue;
                }
                catch {
                }
                if (stillPending) {
                    this.resetMemory(true);
                    this.onAuthenticationLost?.();
                }
                else {
                    void this.checkCookieSessionAfterPeerLogout();
                }
                return;
            }
            if (event.key === this.cookieLogoutKey && this.cookieSession) {
                void this.checkCookieSessionAfterPeerLogout();
                return;
            }
            if (event.key !== null && event.key !== this.storageKey)
                return;
            if (this.usesSecureCookies)
                return;
            let currentRaw;
            try {
                currentRaw = this.readStoredSessionRaw();
            }
            catch {
                if (this.remembered) {
                    this.resetMemory(true);
                    this.onAuthenticationLost?.();
                }
                return;
            }
            if (currentRaw === null) {
                if (this.remembered) {
                    this.resetMemory(true);
                    this.onAuthenticationLost?.();
                }
                return;
            }
            const stored = this.parseStoredSession(currentRaw, true);
            if (!stored) {
                if (this.remembered) {
                    this.resetMemory(true);
                    this.onAuthenticationLost?.();
                }
                void this.removeStoredSessionForIdentity(null, currentRaw);
                return;
            }
            if (stored.refreshing)
                return;
            if (this.remembered)
                this.installStoredSession(stored);
        }
        async checkCookieSessionAfterPeerLogout() {
            const generation = this.authGeneration;
            try {
                await this.withSessionLock(async () => {
                    if (!this.cookieSession || generation !== this.authGeneration)
                        return;
                    try {
                        const reply = await this.send("auth/browser/session", {}, false, "", true);
                        if (generation === this.authGeneration)
                            this.applyBrowserSession(reply);
                    }
                    catch (error) {
                        if (!(error instanceof ApiError) ||
                            (error.status !== 401 && error.code !== "invalid_response"))
                            return;
                        if (generation === this.authGeneration) {
                            this.resetMemory(true);
                            this.onAuthenticationLost?.();
                        }
                    }
                });
            }
            catch {
            }
        }
        resetMemory(invalidatePersistence, preserveRefresh = false) {
            this.authGeneration += 1;
            this.tokenGeneration += 1;
            if (invalidatePersistence)
                this.persistenceGeneration += 1;
            this.bearer = "";
            this.refreshToken = "";
            this.accessExpiresAt = 0;
            this.subject = "";
            this.scopes = null;
            this.remembered = false;
            this.cookieSession = false;
            this.cookieRefreshInFlight = null;
            this.sessionId = null;
            this.refreshInFlight = null;
            this.abortRequests(preserveRefresh);
        }
        abortRequests(preserveRefresh = false) {
            for (const request of this.requests) {
                if (!preserveRefresh || !this.refreshRequests.has(request)) {
                    request.abort();
                    this.requests.delete(request);
                    this.refreshRequests.delete(request);
                }
            }
        }
        async send(route, payload, protectedRoute, bearer, cookieAuth = false) {
            const controller = new AbortController();
            const timer = window.setTimeout(() => controller.abort(), 12_000);
            this.requests.add(controller);
            if (route === "auth/refresh" || route === "auth/browser/refresh")
                this.refreshRequests.add(controller);
            try {
                const headers = {
                    "Content-Type": "application/json",
                };
                if (cookieAuth)
                    headers["X-Amaterasu-CSRF"] = "1";
                else if (protectedRoute)
                    headers.Authorization = `Bearer ${bearer}`;
                const response = await fetch(`${this.base}/v1/${route}`, {
                    method: "POST",
                    headers,
                    body: JSON.stringify(payload),
                    signal: controller.signal,
                    credentials: cookieAuth ? "same-origin" : "omit",
                    cache: "no-store",
                    referrerPolicy: "no-referrer",
                });
                const body = (await response.json());
                if (!response.ok || body.ok !== true)
                    throw this.failure(body, response.status);
                return body;
            }
            catch (error) {
                if (error instanceof ApiError)
                    throw error;
                if (controller.signal.aborted)
                    throw new Error("The request timed out or was cancelled. A change may have completed; refresh before retrying.");
                throw new Error("Could not reach the host. Check its address, connection, and allowed browser origin.");
            }
            finally {
                clearTimeout(timer);
                this.requests.delete(controller);
                this.refreshRequests.delete(controller);
            }
        }
        failure(body, status) {
            const detail = typeof body.error === "object" && body.error
                ? body.error
                : body;
            const code = typeof detail.code === "string" ? detail.code : "request_failed";
            const messages = {
                unauthorized: "Access denied. Sign in again or use a token with the required scope.",
                forbidden: "This account cannot perform that action.",
                rate_limited: "Too many sign-in attempts. Wait a minute before trying again.",
                revision_conflict: "This data changed elsewhere. Refresh before saving again.",
                not_registered: "No settings schema has been registered for this console.",
            };
            return new ApiError(messages[code] ??
                (typeof detail.message === "string"
                    ? detail.message
                    : code.replaceAll("_", " ")), code, status);
        }
    }
    Amaterasu.Api = Api;
})(Amaterasu || (Amaterasu = {}));
var Amaterasu;
(function (Amaterasu) {
    function el(tag, className = "", ...children) {
        const node = document.createElement(tag);
        node.className = className;
        for (const child of children)
            if (child != null)
                node.append(child);
        return node;
    }
    Amaterasu.el = el;
    function button(text, action, kind = "secondary") {
        const node = el("button", `button ${kind}`, text);
        node.type = "button";
        node.addEventListener("click", () => void action());
        return node;
    }
    Amaterasu.button = button;
    function field(label, input, hint) {
        const wrapper = el("label", "field", el("span", "field-label", label), input);
        if (hint)
            wrapper.append(el("small", "muted", hint));
        return wrapper;
    }
    Amaterasu.field = field;
    function input(name, value = "", type = "text") {
        const node = el("input");
        node.name = name;
        node.type = type;
        node.value = value;
        return node;
    }
    Amaterasu.input = input;
    function sectionHeading(title, description, ...actions) {
        return el("header", "page-heading", el("div", "", el("p", "eyebrow", "HOST CONSOLE"), el("h1", "", title), el("p", "muted", description)), el("div", "actions", ...actions));
    }
    Amaterasu.sectionHeading = sectionHeading;
    function empty(title, description, action) {
        return el("div", "empty-state", el("span", "empty-mark", "＋"), el("h3", "", title), el("p", "muted", description), action);
    }
    Amaterasu.empty = empty;
    function errorPanel(error, retry) {
        const box = el("div", "notice error", el("strong", "", "Unable to complete request"), el("p", "", Amaterasu.messageOf(error)));
        box.setAttribute("role", "alert");
        if (retry)
            box.append(button("Try again", retry));
        return box;
    }
    Amaterasu.errorPanel = errorPanel;
    function badge(text, tone = "") {
        return el("span", `badge ${tone}`, text);
    }
    Amaterasu.badge = badge;
    function panel(title, ...children) {
        return el("section", "panel", el("h2", "", title), ...children);
    }
    Amaterasu.panel = panel;
    async function runAction(target, status, action) {
        if (target.disabled)
            return;
        target.disabled = true;
        status.replaceChildren();
        try {
            await action();
        }
        catch (error) {
            if (status.isConnected)
                status.replaceChildren(errorPanel(error));
        }
        finally {
            target.disabled = false;
        }
    }
    Amaterasu.runAction = runAction;
    function table(headers, rows) {
        const head = el("tr");
        for (const name of headers) {
            const cell = el("th", "", name);
            cell.scope = "col";
            head.append(cell);
        }
        const body = el("tbody");
        for (const row of rows)
            body.append(el("tr", "", ...row.map((value) => el("td", "", value))));
        return el("div", "table-scroll", el("table", "", el("thead", "", head), body));
    }
    Amaterasu.table = table;
    function pager(back, next, label) {
        const previous = button("← Previous", () => back?.());
        previous.disabled = !back;
        const following = button("Next →", () => next?.());
        following.disabled = !next;
        return el("div", "pager", el("span", "muted", label), el("div", "actions", previous, following));
    }
    Amaterasu.pager = pager;
})(Amaterasu || (Amaterasu = {}));
(function (Amaterasu) {
    function confirmAction(title, description, label) {
        return new Promise((resolve) => {
            const dialog = Amaterasu.el("dialog", "confirmation");
            dialog.setAttribute("aria-label", title);
            const finish = (accepted) => {
                dialog.close();
                dialog.remove();
                resolve(accepted);
            };
            const cancel = Amaterasu.button("Cancel", () => finish(false));
            dialog.append(Amaterasu.el("h2", "", title), Amaterasu.el("p", "muted", description), Amaterasu.el("div", "actions", cancel, Amaterasu.button(label, () => finish(true), "danger")));
            dialog.addEventListener("cancel", (event) => {
                event.preventDefault();
                finish(false);
            });
            document.body.append(dialog);
            dialog.showModal();
            cancel.focus();
        });
    }
    Amaterasu.confirmAction = confirmAction;
})(Amaterasu || (Amaterasu = {}));
var Amaterasu;
(function (Amaterasu) {
    async function readSteamArtwork(api, reference) {
        if (reference.provider !== "steam" ||
            !Number.isSafeInteger(reference.app_id) ||
            reference.app_id < 1 ||
            reference.app_id > 0xffff_ffff ||
            !/^[0-9]{1,20}$/.test(reference.revision))
            throw new Error("Invalid Steam artwork reference.");
        let offset = 0;
        let totalBytes = null;
        let mimeType = null;
        const chunks = [];
        while (true) {
            const reply = await api.post("steam/artwork_read", {
                app_id: reference.app_id,
                revision: reference.revision,
                offset,
            });
            if (reply.ok !== true ||
                reply.app_id !== reference.app_id ||
                reply.revision !== reference.revision ||
                reply.offset !== offset ||
                !Number.isSafeInteger(reply.total_bytes) ||
                reply.total_bytes < 1 ||
                reply.total_bytes > 16 * 1024 * 1024 ||
                !["image/png", "image/jpeg", "image/webp"].includes(reply.mime_type) ||
                typeof reply.data !== "string" ||
                (totalBytes !== null && totalBytes !== reply.total_bytes) ||
                (mimeType !== null && mimeType !== reply.mime_type))
                throw new Error("Steam artwork changed during transfer.");
            totalBytes = reply.total_bytes;
            mimeType = reply.mime_type;
            const decoded = atob(reply.data);
            if (decoded.length < 1 || decoded.length > 8192 || offset + decoded.length > totalBytes)
                throw new Error("Invalid Steam artwork chunk.");
            const bytes = new Uint8Array(decoded.length);
            for (let index = 0; index < decoded.length; index++)
                bytes[index] = decoded.charCodeAt(index);
            chunks.push(bytes.buffer);
            const end = offset + decoded.length;
            if (reply.next_offset === null) {
                if (end !== totalBytes)
                    throw new Error("Incomplete Steam artwork transfer.");
                return new Blob(chunks, { type: mimeType });
            }
            if (reply.next_offset !== end || end >= totalBytes || end % 8192 !== 0)
                throw new Error("Invalid Steam artwork cursor.");
            offset = end;
        }
    }
    Amaterasu.readSteamArtwork = readSteamArtwork;
    async function readLutrisArtwork(api, reference) {
        if (reference.provider !== "lutris" ||
            !Number.isSafeInteger(reference.game_id) ||
            reference.game_id < 1 ||
            !/^[0-9a-f]{64}$/.test(reference.revision))
            throw new Error("Invalid Lutris artwork reference.");
        let offset = 0;
        let totalBytes = null;
        let mimeType = null;
        const chunks = [];
        while (true) {
            const reply = await api.post("lutris/artwork_read", {
                game_id: reference.game_id,
                revision: reference.revision,
                offset,
            });
            if (reply.ok !== true ||
                reply.game_id !== reference.game_id ||
                reply.revision !== reference.revision ||
                reply.offset !== offset ||
                !Number.isSafeInteger(reply.total_bytes) ||
                reply.total_bytes < 1 ||
                reply.total_bytes > 16 * 1024 * 1024 ||
                !["image/png", "image/jpeg", "image/webp"].includes(reply.mime_type) ||
                typeof reply.data !== "string" ||
                reply.data.length > 10924 ||
                (totalBytes !== null && totalBytes !== reply.total_bytes) ||
                (mimeType !== null && mimeType !== reply.mime_type))
                throw new Error("Lutris artwork changed during transfer.");
            totalBytes = reply.total_bytes;
            mimeType = reply.mime_type;
            const decoded = atob(reply.data);
            if (decoded.length < 1 || decoded.length > 8192 || offset + decoded.length > totalBytes)
                throw new Error("Invalid Lutris artwork chunk.");
            const bytes = new Uint8Array(decoded.length);
            for (let index = 0; index < decoded.length; index++)
                bytes[index] = decoded.charCodeAt(index);
            chunks.push(bytes.buffer);
            const end = offset + decoded.length;
            if (reply.next_offset === null) {
                if (end !== totalBytes)
                    throw new Error("Incomplete Lutris artwork transfer.");
                return new Blob(chunks, { type: mimeType });
            }
            if (reply.next_offset !== end || end >= totalBytes || end % 8192 !== 0)
                throw new Error("Invalid Lutris artwork cursor.");
            offset = end;
        }
    }
    Amaterasu.readLutrisArtwork = readLutrisArtwork;
    function showCatalogArtwork(api, reference, art, content) {
        if (!reference)
            return;
        if (reference.provider === "steam" && !api.allows("steam.read"))
            return;
        if (reference.provider === "lutris" && !api.allows("lutris.read"))
            return;
        const imageRead = reference.provider === "steam"
            ? readSteamArtwork(api, reference)
            : readLutrisArtwork(api, reference);
        void imageRead.then((blob) => {
            if (!content.isConnected || !art.isConnected)
                return;
            const url = URL.createObjectURL(blob);
            const image = Amaterasu.el("img");
            image.alt = "";
            let released = false;
            const observer = new MutationObserver(() => {
                if (!content.isConnected || !art.isConnected)
                    release();
            });
            const release = () => {
                if (released)
                    return;
                released = true;
                observer.disconnect();
                URL.revokeObjectURL(url);
            };
            image.addEventListener("load", release, { once: true });
            image.addEventListener("error", release, { once: true });
            observer.observe(document, { childList: true, subtree: true });
            image.src = url;
            art.prepend(image);
        }).catch(() => {
        });
    }
    Amaterasu.showCatalogArtwork = showCatalogArtwork;
    function audioOverrideSelect(name, current) {
        const select = Amaterasu.el("select");
        select.name = name;
        const choices = [["", "Inherit"], ["true", "Enabled"], ["false", "Disabled"]];
        for (const [value, label] of choices) {
            const option = Amaterasu.el("option", "", label);
            option.value = value;
            select.append(option);
        }
        select.value = current === undefined ? "" : String(current);
        return select;
    }
    const refreshRateOptionValues = ["auto", "disabled", "manual", "prefer_highest"];
    const catalogAutoVirtualFramegenModes = ["enabled", "vrr", "legacy", "disabled"];
    function catalogSavedUnsupportedEnumValueToken(values) {
        let token = "__amaterasu_keep_saved_unsupported_enum_value__";
        while (values.includes(token))
            token += "_";
        return token;
    }
    function catalogFrameLimiterOverrideOptionValues(name, capabilities, current) {
        const values = capabilities[name];
        const options = ["", ...values];
        if (current != null && !values.includes(current))
            options.push(catalogSavedUnsupportedEnumValueToken(values));
        return options;
    }
    Amaterasu.catalogFrameLimiterOverrideOptionValues = catalogFrameLimiterOverrideOptionValues;
    function catalogFrameLimiterOverrideIsDisabled(name, capabilities) {
        return name === "rtss_frame_limit_type" && capabilities.rtss_frame_limit_type.length === 0;
    }
    Amaterasu.catalogFrameLimiterOverrideIsDisabled = catalogFrameLimiterOverrideIsDisabled;
    function catalogFrameLimiterCapabilitySelect(name, current, capabilities) {
        const values = capabilities[name];
        const unsupportedCurrent = current != null && !values.includes(current);
        const savedValueToken = unsupportedCurrent
            ? catalogSavedUnsupportedEnumValueToken(values)
            : undefined;
        const select = Amaterasu.el("select");
        select.name = name;
        const inherit = Amaterasu.el("option", "", "Inherit");
        inherit.value = "";
        select.append(inherit);
        for (const value of values) {
            const option = Amaterasu.el("option", "", value);
            option.value = value;
            select.append(option);
        }
        if (savedValueToken !== undefined) {
            const saved = Amaterasu.el("option", "", `Keep saved value: ${current} (unsupported on this host)`);
            saved.value = savedValueToken;
            select.append(saved);
        }
        select.value = savedValueToken ?? current ?? "";
        if (!catalogFrameLimiterOverrideIsDisabled(name, capabilities))
            return select;
        select.disabled = true;
        select.removeAttribute("name");
        const preservedValue = Amaterasu.input(name, savedValueToken ?? "", "hidden");
        const controls = [
            select,
            preservedValue,
            Amaterasu.el("span", "muted", "Unsupported on this host."),
        ];
        if (savedValueToken !== undefined) {
            const clear = Amaterasu.button("Clear override", () => {
                preservedValue.value = "";
                select.value = "";
                clear.disabled = true;
            }, "quiet");
            clear.type = "button";
            controls.push(clear);
        }
        return Amaterasu.el("div", "catalog-enum-override", ...controls);
    }
    function catalogEnumOverrideSelect(name, current, values) {
        const select = Amaterasu.el("select");
        select.name = name;
        const inherit = Amaterasu.el("option", "", "Inherit");
        inherit.value = "";
        select.append(inherit);
        for (const value of values) {
            const option = Amaterasu.el("option", "", value);
            option.value = value;
            select.append(option);
        }
        if (current != null && !values.includes(current)) {
            const saved = Amaterasu.el("option", "", `Saved value: ${current}`);
            saved.value = current;
            select.append(saved);
        }
        select.value = current ?? "";
        return select;
    }
    function catalogFrameLimiterOverrideField(label, control, hint) {
        if (control instanceof HTMLSelectElement)
            return Amaterasu.field(label, control, hint);
        control.querySelector("select")?.setAttribute("aria-label", label);
        return Amaterasu.el("div", "field", Amaterasu.el("span", "field-label", label), control, Amaterasu.el("small", "muted", hint));
    }
    function catalogFrameLimiterOverrideFormValues(overrides) {
        const text = (value) => value == null ? "" : String(value);
        return {
            frame_limiter_enable: text(overrides?.frame_limiter_enable),
            frame_limiter_fps_limit: text(overrides?.frame_limiter_fps_limit),
            frame_limiter_provider: text(overrides?.frame_limiter_provider),
            frame_limiter_disable_vsync: text(overrides?.frame_limiter_disable_vsync),
            frame_limiter_auto_virtual_framegen: text(overrides?.frame_limiter_auto_virtual_framegen),
            rtss_frame_limit_type: text(overrides?.rtss_frame_limit_type),
            ds4_back_as_touchpad_click: text(overrides?.ds4_back_as_touchpad_click),
        };
    }
    Amaterasu.catalogFrameLimiterOverrideFormValues = catalogFrameLimiterOverrideFormValues;
    function catalogFrameLimiterOverrideFields(overrides, capabilities) {
        const values = catalogFrameLimiterOverrideFormValues(overrides);
        const enable = audioOverrideSelect("frame_limiter_enable", values.frame_limiter_enable === "" ? undefined : values.frame_limiter_enable === "true");
        const fpsLimit = Amaterasu.input("frame_limiter_fps_limit", values.frame_limiter_fps_limit, "number");
        fpsLimit.min = "0";
        fpsLimit.max = "1000";
        fpsLimit.step = "0.001";
        fpsLimit.placeholder = "Inherit";
        const provider = catalogFrameLimiterCapabilitySelect("frame_limiter_provider", values.frame_limiter_provider, capabilities);
        const disableVsync = audioOverrideSelect("frame_limiter_disable_vsync", values.frame_limiter_disable_vsync === ""
            ? undefined
            : values.frame_limiter_disable_vsync === "true");
        const autoVirtualFramegen = catalogEnumOverrideSelect("frame_limiter_auto_virtual_framegen", values.frame_limiter_auto_virtual_framegen, catalogAutoVirtualFramegenModes);
        const rtssFrameLimitType = catalogFrameLimiterCapabilitySelect("rtss_frame_limit_type", values.rtss_frame_limit_type, capabilities);
        const ds4BackAsTouchpadClick = audioOverrideSelect("ds4_back_as_touchpad_click", values.ds4_back_as_touchpad_click === ""
            ? undefined
            : values.ds4_back_as_touchpad_click === "true");
        return [
            Amaterasu.field("Frame limiter", enable, "Blank inherits. Runtime application remains pending."),
            Amaterasu.field("Frame limiter FPS limit", fpsLimit, "Blank inherits. Use 0–1000 Hz in 0.001 Hz steps."),
            catalogFrameLimiterOverrideField("Frame limiter provider", provider, "Choose a provider supported by this host; blank inherits."),
            Amaterasu.field("Disable VSync for RTSS", disableVsync, "Blank inherits the next scope."),
            Amaterasu.field("Auto virtual frame generation mode", autoVirtualFramegen, "Blank inherits. Choose enabled, vrr, legacy, or disabled."),
            catalogFrameLimiterOverrideField("RTSS frame limit type", rtssFrameLimitType, "Blank inherits. Available RTSS choices depend on host support."),
            Amaterasu.field("DS4 Back as touchpad click", ds4BackAsTouchpadClick, "Only applies to a DS4 profile. Blank inherits; explicit false is preserved."),
        ];
    }
    function parseCatalogFrameLimiterEnum(value, allowedValues, currentValue, label) {
        if (value === "")
            return null;
        if (currentValue != null &&
            !allowedValues.includes(currentValue) &&
            value === catalogSavedUnsupportedEnumValueToken(allowedValues))
            return currentValue;
        if (!allowedValues.includes(value))
            throw new Error(`${label} must be one of the supported values.`);
        return value;
    }
    function parseCatalogFrameLimiterFpsLimit(value) {
        const trimmed = value.trim();
        if (trimmed === "")
            return null;
        if (!/^(?:[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+))(?:[eE][+-]?[0-9]+)?$/.test(trimmed))
            throw new Error("Frame limiter FPS limit must be from 0 to 1000 in 0.001 Hz steps.");
        const fpsLimit = Number(trimmed);
        if (!Number.isFinite(fpsLimit) || fpsLimit < 0 || fpsLimit > 1000)
            throw new Error("Frame limiter FPS limit must be from 0 to 1000 in 0.001 Hz steps.");
        const millihertz = fpsLimit * 1000;
        const roundingTolerance = Number.EPSILON * Math.max(Math.abs(millihertz), 1) * 4;
        if (Math.abs(millihertz - Math.round(millihertz)) > roundingTolerance)
            throw new Error("Frame limiter FPS limit must be from 0 to 1000 in 0.001 Hz steps.");
        return fpsLimit;
    }
    function parseCatalogFrameLimiterOverrides(data, capabilities, currentOverrides) {
        const text = (name) => String(data.get(name) ?? "");
        return {
            frame_limiter_enable: Amaterasu.parseInputBooleanOverride(text("frame_limiter_enable"), "Frame limiter"),
            frame_limiter_fps_limit: parseCatalogFrameLimiterFpsLimit(text("frame_limiter_fps_limit")),
            frame_limiter_provider: parseCatalogFrameLimiterEnum(text("frame_limiter_provider"), capabilities.frame_limiter_provider, currentOverrides?.frame_limiter_provider, "Frame limiter provider"),
            frame_limiter_disable_vsync: Amaterasu.parseInputBooleanOverride(text("frame_limiter_disable_vsync"), "Disable VSync for RTSS"),
            frame_limiter_auto_virtual_framegen: parseCatalogFrameLimiterEnum(text("frame_limiter_auto_virtual_framegen"), catalogAutoVirtualFramegenModes, undefined, "Auto virtual frame generation mode"),
            rtss_frame_limit_type: parseCatalogFrameLimiterEnum(text("rtss_frame_limit_type"), capabilities.rtss_frame_limit_type, currentOverrides?.rtss_frame_limit_type, "RTSS frame limit type"),
            ds4_back_as_touchpad_click: Amaterasu.parseInputBooleanOverride(text("ds4_back_as_touchpad_click"), "DS4 Back as touchpad click"),
        };
    }
    Amaterasu.parseCatalogFrameLimiterOverrides = parseCatalogFrameLimiterOverrides;
    function catalogRefreshRateOptionText(value) {
        return value ?? "";
    }
    Amaterasu.catalogRefreshRateOptionText = catalogRefreshRateOptionText;
    function parseCatalogRefreshRateOptionOverride(data) {
        return Amaterasu.parseRefreshRateOptionOverride(String(data.get("dd_refresh_rate_option") ?? ""));
    }
    Amaterasu.parseCatalogRefreshRateOptionOverride = parseCatalogRefreshRateOptionOverride;
    function catalogManualRefreshRateCompanionVisible(value) {
        return Amaterasu.usesManualRefreshRateCompanion(value);
    }
    Amaterasu.catalogManualRefreshRateCompanionVisible = catalogManualRefreshRateCompanionVisible;
    function catalogPrefer10BitSdrOverrideValue(value) {
        if (value == null)
            return null;
        if (typeof value !== "boolean")
            throw new Error("Saved 10-bit SDR preference must be a boolean or null.");
        return value;
    }
    Amaterasu.catalogPrefer10BitSdrOverrideValue = catalogPrefer10BitSdrOverrideValue;
    function catalogRefreshRateOptionFields(current) {
        const option = catalogEnumOverrideSelect("dd_refresh_rate_option", catalogRefreshRateOptionText(current), refreshRateOptionValues);
        const manualCompanionNotice = Amaterasu.el("p", "muted", "Manual mode uses the separate dd_manual_refresh_rate setting.");
        const updateManualCompanionNotice = () => {
            manualCompanionNotice.hidden = !catalogManualRefreshRateCompanionVisible(option.value);
        };
        option.addEventListener("change", updateManualCompanionNotice);
        updateManualCompanionNotice();
        return [
            Amaterasu.field("Refresh rate option", option, "Blank inherits the next scope. Choose auto, disabled, manual, or prefer_highest."),
            manualCompanionNotice,
        ];
    }
    function inputGamepadOverrideSelect(name, current, status) {
        const select = Amaterasu.el("select");
        select.name = name;
        const inherit = Amaterasu.el("option", "", "Inherit application/host");
        inherit.value = "";
        select.append(inherit);
        for (const choice of Amaterasu.inputGamepadProfileChoices(current ?? "", status.supported_profiles, status.recognized_profiles)) {
            const option = Amaterasu.el("option", "", choice.label);
            option.value = choice.value;
            select.append(option);
        }
        select.value = current ?? "";
        return select;
    }
    function audioSinkOverrideControls(current) {
        const mode = Amaterasu.el("select");
        mode.name = "audio_sink_mode";
        for (const [value, label] of [
            ["inherit", "Inherit host endpoint"],
            ["automatic", "Automatic/default endpoint"],
            ["specific", "Specific endpoint ID"],
        ]) {
            const option = Amaterasu.el("option", "", label);
            option.value = value ?? "";
            mode.append(option);
        }
        mode.value = current === undefined ? "inherit" : current === "" ? "automatic" : "specific";
        const id = Amaterasu.input("audio_sink_id", current ?? "");
        id.maxLength = 4096;
        const update = () => { id.disabled = mode.value !== "specific"; };
        mode.addEventListener("change", update);
        update();
        return {
            mode,
            id,
            controls: Amaterasu.el("div", "form-grid", Amaterasu.field("Audio capture endpoint", mode, "Inherit the host, choose automatic, or use an exact endpoint ID."), Amaterasu.field("Specific endpoint ID", id, "Used only when a specific endpoint is selected.")),
        };
    }
    function virtualSinkOverrideControls(current) {
        const mode = Amaterasu.el("select");
        mode.name = "virtual_sink_mode";
        for (const [value, label] of [
            ["inherit", "Inherit application/host"],
            ["automatic", "Clear inherited virtual sink"],
            ["specific", "Specific virtual sink selector"],
        ]) {
            const option = Amaterasu.el("option", "", label);
            option.value = value ?? "";
            mode.append(option);
        }
        mode.value = current === undefined ? "inherit" : current === "" ? "automatic" : "specific";
        const selector = Amaterasu.input("virtual_sink_selector", current ?? "");
        selector.maxLength = 4096;
        selector.placeholder = "Exact ID or device name";
        const update = () => { selector.disabled = mode.value !== "specific"; };
        mode.addEventListener("change", update);
        update();
        return {
            mode,
            selector,
            controls: Amaterasu.el("div", "form-grid", Amaterasu.field("Virtual audio sink", mode, "Inherit the next scope, clear inherited selection, or choose a virtual device."), Amaterasu.field("Virtual sink ID or name", selector, "Used for a specific device; Windows matches IDs and names exactly.")),
        };
    }
    function renderCatalog(context) {
        const { content, api, notify } = context;
        const search = Amaterasu.input("search");
        search.type = "search";
        search.placeholder = "Search names, descriptions, tags";
        search.setAttribute("aria-label", "Search catalog");
        const searchForm = Amaterasu.el("form", "search-form", search);
        const searchButton = Amaterasu.el("button", "button secondary", "Search");
        searchButton.type = "submit";
        searchForm.append(searchButton);
        const results = Amaterasu.el("div");
        const editor = Amaterasu.el("div");
        const status = Amaterasu.el("div");
        let query = "";
        let revision = 0;
        let cursors = [null];
        let page = 0;
        let generation = 0;
        let configOverrideCapabilities = null;
        const add = Amaterasu.button("+ Add application", () => {
            if (configOverrideCapabilities)
                showEditor(undefined, configOverrideCapabilities);
        }, "primary");
        add.disabled = true;
        content.append(Amaterasu.sectionHeading("Your library", "A durable catalog of the applications on your host.", add), searchForm, status, editor, results);
        async function load() {
            const current = ++generation;
            results.setAttribute("aria-busy", "true");
            try {
                const reply = await api.post(query ? "catalog/search" : "catalog/list", { ...(query ? { query } : {}), cursor: cursors[page], limit: 12 });
                if (!content.isConnected || current !== generation)
                    return;
                revision = reply.revision;
                configOverrideCapabilities = reply.config_override_capabilities;
                add.disabled = !api.allows("catalog.write");
                const grid = Amaterasu.el("div", "catalog-grid");
                for (const row of reply.entries)
                    grid.append(card(row, reply.config_override_capabilities));
                results.replaceChildren(Amaterasu.el("div", "section-meta", Amaterasu.el("span", "", `${reply.entries.length} applications on this page`), Amaterasu.badge(`Revision ${revision}`)), reply.entries.length
                    ? grid
                    : Amaterasu.empty(query
                        ? "No matches on this page"
                        : "Make room for your favorites", reply.next_cursor
                        ? "Search continues on the next page."
                        : query
                            ? "Try a different name or tag."
                            : "Add your first application to start building this host’s library."), Amaterasu.pager(page
                    ? () => {
                        page--;
                        void load();
                    }
                    : null, reply.next_cursor != null
                    ? () => {
                        cursors = cursors.slice(0, page + 1);
                        cursors.push(reply.next_cursor);
                        page++;
                        void load();
                    }
                    : null, `Page ${page + 1}`));
            }
            catch (error) {
                if (content.isConnected && current === generation)
                    results.replaceChildren(Amaterasu.errorPanel(error, reset));
            }
            finally {
                results.removeAttribute("aria-busy");
            }
        }
        function reset() {
            cursors = [null];
            page = 0;
            void load();
        }
        searchForm.addEventListener("submit", (event) => {
            event.preventDefault();
            query = search.value.trim();
            reset();
        });
        function card(row, capabilities) {
            const providerLaunch = row.entry.launch.kind === "playnite";
            const edit = Amaterasu.button("Edit", () => showEditor(row, capabilities));
            edit.disabled = !api.allows("catalog.write") || providerLaunch;
            if (providerLaunch)
                edit.title = "Playnite launch targets are managed by the provider.";
            const actions = [edit];
            if (providerLaunch) {
                const configure = Amaterasu.button("Streaming settings", () => showProviderOverrideEditor(row, capabilities), "secondary");
                configure.disabled = !api.allows("catalog.write");
                actions.unshift(configure);
            }
            const remove = Amaterasu.button("Delete", async () => {
                if (!(await Amaterasu.confirmAction(`Delete ${row.entry.name}?`, "This removes its catalog entry. It does not remove files or stop a running application.", "Delete entry")))
                    return;
                await Amaterasu.runAction(remove, status, async () => {
                    await api.post("catalog/delete", {
                        id: row.id,
                        expected_revision: revision,
                    });
                    notify("Application removed from the catalog.");
                    reset();
                });
            }, "quiet danger-text");
            remove.disabled = !api.allows("catalog.write");
            actions.push(remove);
            const art = Amaterasu.el("div", "app-art", Amaterasu.el("span", "", row.entry.name.slice(0, 2).toUpperCase()), Amaterasu.badge(row.entry.launch.kind === "executable"
                ? "Executable"
                : providerLaunch
                    ? "Playnite"
                    : "Application"));
            showCatalogArtwork(api, row.entry.artwork, art, content);
            return Amaterasu.el("article", "app-card", art, Amaterasu.el("div", "app-details", Amaterasu.el("h2", "", row.entry.name), Amaterasu.el("p", "muted description", row.entry.description || "No description added."), Amaterasu.el("div", "tags", ...(row.entry.tags ?? []).map((tag) => Amaterasu.badge(tag))), Amaterasu.el("p", "path", Amaterasu.launchPath(row.entry)), Amaterasu.el("div", "card-footer", ...actions)));
        }
        function showProviderOverrideEditor(row, capabilities) {
            editor.replaceChildren(Amaterasu.el("p", "muted", "Loading input profiles…"));
            void loadInputRuntimeStatus().then((status) => {
                if (context.content.isConnected)
                    renderProviderOverrideEditor(row, status, capabilities);
            });
        }
        async function loadInputRuntimeStatus() {
            try {
                return await api.post("input/status");
            }
            catch {
                return {
                    ok: false,
                    gamepad: "auto",
                    supported_profiles: ["auto"],
                    recognized_profiles: ["auto"],
                    effective_gamepad_profile: null,
                    gamepad_unavailable_reason: null,
                    controller_input: false,
                    controller_enabled: true,
                    controller_cleanup_required: false,
                    lease: null,
                };
            }
        }
        function renderProviderOverrideEditor(row, runtimeStatus, capabilities) {
            let editRevision = revision;
            const currentFec = row.entry.config_overrides?.fec_percentage;
            const fec = Amaterasu.input("fec_percentage", currentFec === undefined ? "" : String(currentFec), "number");
            fec.min = "1";
            fec.max = "255";
            fec.step = "1";
            fec.placeholder = "Inherit";
            fec.setAttribute("aria-label", `FEC percentage override for ${row.entry.name}`);
            const frameLimiterFields = catalogFrameLimiterOverrideFields(row.entry.config_overrides, capabilities);
            const streamAudio = audioOverrideSelect("stream_audio", row.entry.config_overrides?.stream_audio);
            const audioSink = audioSinkOverrideControls(row.entry.config_overrides?.audio_sink);
            const virtualSink = virtualSinkOverrideControls(row.entry.config_overrides?.virtual_sink);
            const captureOnly = audioOverrideSelect("audio_sink_capture_only", row.entry.config_overrides?.audio_sink_capture_only);
            const backButtonTimeout = Amaterasu.input("back_button_timeout", row.entry.config_overrides?.back_button_timeout?.toString() ?? "", "number");
            backButtonTimeout.min = String(-0x8000_0000);
            backButtonTimeout.max = String(0x7fff_ffff);
            backButtonTimeout.step = "1";
            backButtonTimeout.placeholder = "Inherit";
            backButtonTimeout.setAttribute("aria-label", `Back button timeout override for ${row.entry.name}`);
            const controller = audioOverrideSelect("controller", row.entry.config_overrides?.controller);
            const gamepad = inputGamepadOverrideSelect("gamepad", row.entry.config_overrides?.gamepad, runtimeStatus);
            const keyboard = audioOverrideSelect("keyboard", row.entry.config_overrides?.keyboard);
            const mouse = audioOverrideSelect("mouse", row.entry.config_overrides?.mouse);
            const refreshRateOptionFields = catalogRefreshRateOptionFields(row.entry.config_overrides?.dd_refresh_rate_option);
            const form = Amaterasu.el("form", "editor-form", Amaterasu.field("FEC percentage", fec, "Leave blank to inherit the global setting. Supported values are 1–255."), ...frameLimiterFields, ...refreshRateOptionFields, Amaterasu.field("Audio streaming", streamAudio, "Inherit the host setting, or choose whether this game streams audio."), audioSink.controls, virtualSink.controls, Amaterasu.field("Capture audio without changing default outputs", captureOnly, "Inherit the host setting, enable capture only, or allow Windows default-output routing."), Amaterasu.field("Back button timeout (milliseconds)", backButtonTimeout, "Leave blank to inherit. Use -1 to disable the timeout; other signed 32-bit values set the duration."), Amaterasu.field("Controller input", controller, "Inherit the next scope, or enable/disable gamepad input for this application."), Amaterasu.field("Gamepad profile", gamepad, "Inherit the next scope. Recognized profiles unavailable on this backend stay selectable and are labeled unavailable."), Amaterasu.field("Keyboard input", keyboard, "Inherit the next scope, or enable/disable keyboard input for this application."), Amaterasu.field("Mouse input", mouse, "Inherit the next scope, or enable/disable mouse input for this application."));
            const feedback = Amaterasu.el("div");
            const save = Amaterasu.el("button", "button primary", "Save streaming settings");
            save.type = "submit";
            const cancel = Amaterasu.button("Cancel", () => editor.replaceChildren());
            form.append(feedback, Amaterasu.el("div", "actions", save, cancel));
            form.addEventListener("submit", (event) => {
                event.preventDefault();
                if (!form.reportValidity())
                    return;
                void Amaterasu.runAction(save, feedback, async () => {
                    const config_overrides = {
                        ...Amaterasu.providerConfigOverrides(fec.value, streamAudio.value, audioSink.mode.value, audioSink.id.value, captureOnly.value, backButtonTimeout.value, virtualSink.mode.value, virtualSink.selector.value, controller.value, gamepad.value, keyboard.value, mouse.value, runtimeStatus.recognized_profiles),
                        ...parseCatalogFrameLimiterOverrides(new FormData(form), capabilities, row.entry.config_overrides),
                        dd_refresh_rate_option: parseCatalogRefreshRateOptionOverride(new FormData(form)),
                    };
                    try {
                        const reply = await api.post("catalog/update_provider_overrides", {
                            id: row.id,
                            expected_revision: editRevision,
                            config_overrides,
                        });
                        editRevision = reply.revision;
                        editor.replaceChildren();
                        notify(reply.replayed
                            ? `The earlier streaming override save for ${row.entry.name} is confirmed.`
                            : reply.changed
                                ? `Streaming override saved for ${row.entry.name}.`
                                : `Streaming override for ${row.entry.name} is unchanged.`);
                        await load();
                    }
                    catch (error) {
                        if (error instanceof Amaterasu.ApiError && error.code === "revision_conflict") {
                            editor.replaceChildren();
                            await load();
                            notify(`Streaming settings for ${row.entry.name} changed elsewhere. The latest value was reloaded; reopen the editor to review it.`);
                            return;
                        }
                        throw error;
                    }
                });
            });
            editor.replaceChildren(Amaterasu.panel("Playnite streaming settings", Amaterasu.el("p", "muted", `${row.entry.name} · Playnite game ${row.entry.launch.kind === "playnite" ? row.entry.launch.game_id : ""}`), Amaterasu.el("p", "muted", "The Playnite game identity and provider-managed launch metadata stay unchanged."), form));
            editor.scrollIntoView({ behavior: "smooth", block: "start" });
            fec.focus();
        }
        function showEditor(row, capabilities) {
            void loadInputRuntimeStatus().then((runtimeStatus) => {
                if (context.content.isConnected)
                    renderEditor(row, runtimeStatus, capabilities);
            });
        }
        function renderEditor(row, runtimeStatus, capabilities) {
            const editRevision = revision;
            const form = entryForm(row?.entry, runtimeStatus, capabilities);
            const feedback = Amaterasu.el("div");
            const save = Amaterasu.el("button", "button primary", row ? "Save changes" : "Add application");
            save.type = "submit";
            const cancel = Amaterasu.button("Cancel", () => editor.replaceChildren());
            form.append(feedback, Amaterasu.el("div", "actions", save, cancel));
            form.addEventListener("submit", (event) => {
                event.preventDefault();
                if (!form.reportValidity())
                    return;
                void Amaterasu.runAction(save, feedback, async () => {
                    const entry = Amaterasu.entryFromForm(new FormData(form), runtimeStatus.recognized_profiles);
                    entry.config_overrides = {
                        ...(entry.config_overrides ?? {}),
                        ...parseCatalogFrameLimiterOverrides(new FormData(form), capabilities, row?.entry.config_overrides),
                    };
                    await api.post(row ? "catalog/update" : "catalog/create", {
                        ...(row ? { id: row.id } : {}),
                        entry,
                        expected_revision: editRevision,
                    });
                    editor.replaceChildren();
                    notify(row ? "Application updated." : "Application added.");
                    reset();
                });
            });
            editor.replaceChildren(Amaterasu.panel(row ? "Edit application" : "Add application", Amaterasu.el("p", "muted", "Save launch metadata for this host. Adding an entry does not launch it."), form));
            editor.scrollIntoView({ behavior: "smooth", block: "start" });
            form.querySelector("input")?.focus();
        }
        void load();
    }
    Amaterasu.renderCatalog = renderCatalog;
    function entryForm(entry, runtimeStatus, capabilities) {
        const form = Amaterasu.el("form", "editor-form");
        const name = Amaterasu.input("name", entry?.name ?? "");
        name.required = true;
        name.maxLength = 160;
        const description = Amaterasu.el("textarea");
        description.name = "description";
        description.value = entry?.description ?? "";
        description.maxLength = 1024;
        description.rows = 2;
        const tags = Amaterasu.input("tags", entry?.tags?.join(", ") ?? "");
        const excludeGlobalPreparation = Amaterasu.el("input");
        excludeGlobalPreparation.type = "checkbox";
        excludeGlobalPreparation.name = "exclude_global_prep_cmd";
        excludeGlobalPreparation.checked = entry?.exclude_global_prep_cmd ?? false;
        const kind = Amaterasu.el("select");
        kind.name = "kind";
        for (const [value, text] of [
            ["application", "Application / bundle"],
            ["executable", "Executable with arguments"],
            ["playnite_fullscreen", "Playnite Fullscreen"],
        ]) {
            const option = Amaterasu.el("option", "", text);
            option.value = value ?? "";
            kind.append(option);
        }
        kind.value = entry?.launch.kind ?? "application";
        const path = Amaterasu.input("path", entry ? Amaterasu.launchPath(entry) : "");
        path.required = true;
        path.placeholder = "/Applications/Example.app";
        const executableLaunch = entry?.launch.kind === "executable" ? entry.launch : undefined;
        const argumentsField = Amaterasu.el("textarea");
        argumentsField.name = "arguments";
        argumentsField.rows = 2;
        argumentsField.value = JSON.stringify(executableLaunch?.arguments ?? []);
        const directory = Amaterasu.input("working_directory", entry?.launch.kind === "executable"
            ? (entry.launch.working_directory ?? "")
            : "");
        const autoDetach = Amaterasu.el("input");
        autoDetach.type = "checkbox";
        autoDetach.name = "auto_detach";
        autoDetach.checked = executableLaunch?.auto_detach ?? true;
        const waitAll = Amaterasu.el("input");
        waitAll.type = "checkbox";
        waitAll.name = "wait_all";
        waitAll.checked = executableLaunch?.wait_all ?? true;
        const exitTimeout = Amaterasu.input("exit_timeout_seconds", String(executableLaunch?.exit_timeout_seconds ?? 5), "number");
        exitTimeout.min = "0";
        exitTimeout.max = "300";
        exitTimeout.step = "1";
        const detached = Amaterasu.el("textarea");
        detached.name = "detached";
        detached.rows = 3;
        detached.value = executableLaunch?.detached?.join("\n") ?? "";
        const preparation = Amaterasu.input("preparation", JSON.stringify(executableLaunch?.preparation ?? []), "hidden");
        const fec = Amaterasu.input("fec_percentage", entry?.config_overrides?.fec_percentage?.toString() ?? "", "number");
        fec.min = "1";
        fec.max = "255";
        fec.step = "1";
        const frameLimiterFields = catalogFrameLimiterOverrideFields(entry?.config_overrides, capabilities);
        const streamAudio = audioOverrideSelect("stream_audio", entry?.config_overrides?.stream_audio);
        const audioSink = audioSinkOverrideControls(entry?.config_overrides?.audio_sink);
        const virtualSink = virtualSinkOverrideControls(entry?.config_overrides?.virtual_sink);
        const captureOnly = audioOverrideSelect("audio_sink_capture_only", entry?.config_overrides?.audio_sink_capture_only);
        const backButtonTimeout = Amaterasu.input("back_button_timeout", entry?.config_overrides?.back_button_timeout?.toString() ?? "", "number");
        backButtonTimeout.min = String(-0x8000_0000);
        backButtonTimeout.max = String(0x7fff_ffff);
        backButtonTimeout.step = "1";
        backButtonTimeout.placeholder = "Inherit";
        const controller = audioOverrideSelect("controller", entry?.config_overrides?.controller);
        const gamepad = inputGamepadOverrideSelect("gamepad", entry?.config_overrides?.gamepad, runtimeStatus);
        const keyboard = audioOverrideSelect("keyboard", entry?.config_overrides?.keyboard);
        const mouse = audioOverrideSelect("mouse", entry?.config_overrides?.mouse);
        const refreshRateOptionFields = catalogRefreshRateOptionFields(entry?.config_overrides?.dd_refresh_rate_option);
        const prefer10BitSdr = audioOverrideSelect("prefer_10bit_sdr", catalogPrefer10BitSdrOverrideValue(entry?.config_overrides?.prefer_10bit_sdr) ?? undefined);
        const steamId = Amaterasu.input("steam_id", entry?.steam_id ?? "");
        steamId.maxLength = 10;
        const steamInstallDir = Amaterasu.input("steam_install_dir", entry?.steam_install_dir ?? "");
        steamInstallDir.maxLength = 1024;
        const fullscreenTimeout = Amaterasu.input("fullscreen_exit_timeout_seconds", String(entry?.launch.kind === "playnite_fullscreen" ? entry.launch.exit_timeout_seconds : 10), "number");
        fullscreenTimeout.min = "0";
        fullscreenTimeout.max = "300";
        fullscreenTimeout.step = "1";
        fullscreenTimeout.required = true;
        const processPolicy = Amaterasu.el("div", "form-grid", Amaterasu.field("Auto detach", autoDetach, "Retain a quick successful launcher."), Amaterasu.field("Wait for all processes", waitAll, "Keep the app active while its process group is running."), Amaterasu.field("Exit timeout (seconds)", exitTimeout, "0 skips the graceful shutdown window; maximum 300."), Amaterasu.field("Detached commands", detached, "One ordered command per line; up to 16."));
        const executable = Amaterasu.el("div", "form-grid", Amaterasu.field("Arguments", argumentsField, 'A JSON array of literal arguments. Example: ["--fullscreen"]'), Amaterasu.field("Working directory", directory, "Optional absolute path."), processPolicy, preparation);
        const pathField = Amaterasu.field("Absolute application path", path);
        const steamIdField = Amaterasu.field("Steam app ID", steamId, "Optional canonical Steam app ID for provider-aware process tracking.");
        const steamInstallDirField = Amaterasu.field("Steam install directory", steamInstallDir, "Optional absolute game directory; Linux tracks only newly launched processes inside it.");
        const fullscreen = Amaterasu.field("Exit timeout (seconds)", fullscreenTimeout, "Graceful Playnite Fullscreen close window, from 0 to 300 seconds.");
        const showKind = () => {
            executable.hidden = kind.value !== "executable";
            const isFullscreen = kind.value === "playnite_fullscreen";
            fullscreen.hidden = !isFullscreen;
            pathField.hidden = isFullscreen;
            path.required = !isFullscreen;
            steamIdField.hidden = isFullscreen;
            steamInstallDirField.hidden = isFullscreen;
            path.placeholder =
                kind.value === "executable"
                    ? "/opt/games/example"
                    : "/Applications/Example.app";
        };
        kind.addEventListener("change", showKind);
        form.append(Amaterasu.el("div", "form-grid", Amaterasu.field("Name", name), Amaterasu.field("Launch type", kind)), Amaterasu.field("Description", description), Amaterasu.field("Tags", tags, "Separate tags with commas."), Amaterasu.field("Skip global preparation commands", excludeGlobalPreparation, "Do not run the host-wide preparation commands for this application."), pathField, Amaterasu.field("FEC percentage", fec, "Leave blank to inherit the global streaming setting; supported values are 1–255."), ...frameLimiterFields, ...refreshRateOptionFields, Amaterasu.field("10-bit SDR preference", prefer10BitSdr, "Inherit the next setting scope, or set an explicit preference for this application."), Amaterasu.field("Audio streaming", streamAudio, "Inherit the host setting, or choose whether this application streams audio."), audioSink.controls, virtualSink.controls, Amaterasu.field("Capture audio without changing default outputs", captureOnly, "Inherit the host setting, enable capture only, or allow Windows default-output routing."), Amaterasu.field("Back button timeout (milliseconds)", backButtonTimeout, "Leave blank to inherit. Use -1 to disable the timeout; other signed 32-bit values set the duration."), Amaterasu.field("Controller input", controller, "Inherit the next scope, or enable/disable gamepad input for this application."), Amaterasu.field("Gamepad profile", gamepad, "Inherit the next scope. Recognized profiles unavailable on this backend stay selectable and are labeled unavailable."), Amaterasu.field("Keyboard input", keyboard, "Inherit the next scope, or enable/disable keyboard input for this application."), Amaterasu.field("Mouse input", mouse, "Inherit the next scope, or enable/disable mouse input for this application."), steamIdField, steamInstallDirField, fullscreen, executable);
        showKind();
        return form;
    }
})(Amaterasu || (Amaterasu = {}));
var Amaterasu;
(function (Amaterasu) {
    const maxConfiguredAuthTtlSeconds = 0x7fff_ffff;
    const maxInputKeyRepeatDelayMs = 0x7fff_ffff;
    const minInputBackButtonTimeoutMs = -0x8000_0000;
    const maxInputBackButtonTimeoutMs = 0x7fff_ffff;
    const maxHistoryLimit = 0x7fff_ffff;
    function parseVirtualSinkSelection(enabled, selector) {
        if (typeof enabled !== "boolean" || typeof selector !== "string")
            throw new Error("Virtual sink selection is invalid.");
        if (new TextEncoder().encode(selector).length > 4096 || /[\u0000-\u001f\u007f-\u009f]/u.test(selector))
            throw new Error("Virtual sink selector must be at most 4096 UTF-8 bytes without control characters.");
        return enabled ? selector : null;
    }
    Amaterasu.parseVirtualSinkSelection = parseVirtualSinkSelection;
    function parseHistorySettings(enabled, ttlDays, sizeLimitMb) {
        if (typeof enabled !== "boolean" ||
            typeof ttlDays !== "number" ||
            !Number.isSafeInteger(ttlDays) ||
            ttlDays < 0 ||
            ttlDays > maxHistoryLimit ||
            typeof sizeLimitMb !== "number" ||
            !Number.isSafeInteger(sizeLimitMb) ||
            sizeLimitMb < 0 ||
            sizeLimitMb > maxHistoryLimit)
            throw new Error("History limits must be whole numbers from 0 to 2147483647.");
        return { enabled, ttl_days: ttlDays, db_size_limit_mb: sizeLimitMb };
    }
    Amaterasu.parseHistorySettings = parseHistorySettings;
    function parseHistorySettingsReply(value) {
        if (value === null || typeof value !== "object" || Array.isArray(value))
            throw new Error("Session history settings reply is invalid.");
        const reply = value;
        if (reply.ok !== true)
            throw new Error("Session history settings reply must report success.");
        if (reply.settings === null || typeof reply.settings !== "object" || Array.isArray(reply.settings))
            throw new Error("Session history settings reply contains invalid settings.");
        if (typeof reply.revision !== "number" ||
            !Number.isSafeInteger(reply.revision) ||
            reply.revision < 0)
            throw new Error("Session history settings revision must be a nonnegative safe integer.");
        if (typeof reply.effective_enabled !== "boolean")
            throw new Error("Session history effective enabled state must be a boolean.");
        const settings = reply.settings;
        return {
            ok: true,
            settings: parseHistorySettings(settings.enabled, settings.ttl_days, settings.db_size_limit_mb),
            revision: reply.revision,
            effective_enabled: reply.effective_enabled,
        };
    }
    Amaterasu.parseHistorySettingsReply = parseHistorySettingsReply;
    function parseAuthTokenLifetimes(sessionTokenTtlSeconds, rememberMeRefreshTokenTtlSeconds) {
        if (!Number.isSafeInteger(sessionTokenTtlSeconds) ||
            sessionTokenTtlSeconds < 1 ||
            sessionTokenTtlSeconds > maxConfiguredAuthTtlSeconds ||
            !Number.isSafeInteger(rememberMeRefreshTokenTtlSeconds) ||
            rememberMeRefreshTokenTtlSeconds < 1 ||
            rememberMeRefreshTokenTtlSeconds > maxConfiguredAuthTtlSeconds)
            throw new Error("Authentication lifetimes must be whole seconds from 1 to 2147483647.");
        return {
            session_token_ttl_seconds: sessionTokenTtlSeconds,
            remember_me_refresh_token_ttl_seconds: rememberMeRefreshTokenTtlSeconds,
        };
    }
    Amaterasu.parseAuthTokenLifetimes = parseAuthTokenLifetimes;
    async function saveHistorySettings(api, current, enabled, ttlDays, sizeLimitMb) {
        const settings = parseHistorySettings(enabled, ttlDays, sizeLimitMb);
        const updateReply = await api.post("history/settings_update", {
            settings,
            expected_revision: current.revision,
        });
        if (updateReply && typeof updateReply === "object" &&
            updateReply.code === "revision_conflict")
            throw new Error("Session history settings changed in another editor. Refresh and try again.");
        const updated = parseHistorySettingsReply(updateReply);
        if (updated.revision !== current.revision + 1 ||
            updated.settings.enabled !== settings.enabled ||
            updated.settings.ttl_days !== settings.ttl_days ||
            updated.settings.db_size_limit_mb !== settings.db_size_limit_mb)
            throw new Error("Session history update did not confirm the requested settings. Refresh and check the current values.");
        const saved = parseHistorySettingsReply(await api.post("history/settings_get"));
        if (saved.revision !== current.revision + 1 ||
            saved.settings.enabled !== settings.enabled ||
            saved.settings.ttl_days !== settings.ttl_days ||
            saved.settings.db_size_limit_mb !== settings.db_size_limit_mb)
            throw new Error("Session history settings could not be confirmed after saving. Refresh and check the current values.");
        return saved;
    }
    Amaterasu.saveHistorySettings = saveHistorySettings;
    async function readHistorySettingsState(api) {
        const settings = parseHistorySettingsReply(await api.post("history/settings_get"));
        return { settings, runtime: await readHistoryRuntime(api) };
    }
    Amaterasu.readHistorySettingsState = readHistorySettingsState;
    class HistorySettingsLoadGate {
        generation = 0;
        invalidate() {
            this.generation += 1;
        }
        async read(api) {
            const generation = ++this.generation;
            try {
                const state = await readHistorySettingsState(api);
                return generation === this.generation ? state : null;
            }
            catch (error) {
                if (generation !== this.generation)
                    return null;
                throw error;
            }
        }
    }
    Amaterasu.HistorySettingsLoadGate = HistorySettingsLoadGate;
    async function readHistoryRuntime(api) {
        try {
            return Amaterasu.parseHistoryRuntimeStatus(await api.post("history/history_status"));
        }
        catch {
            return null;
        }
    }
    async function saveHistorySettingsState(api, current, enabled, ttlDays, sizeLimitMb) {
        const settings = await saveHistorySettings(api, current, enabled, ttlDays, sizeLimitMb);
        return { settings, runtime: await readHistoryRuntime(api) };
    }
    Amaterasu.saveHistorySettingsState = saveHistorySettingsState;
    function historySettingsRuntimeDescription(settings, runtime) {
        const saved = `Saved history settings revision ${settings.revision} confirmed.`;
        if (runtime === null)
            return `${saved} Runtime status is unavailable; recording is not confirmed.`;
        if (!runtime.available)
            return `${saved} History storage is unavailable; runtime application is not confirmed.`;
        if (!runtime.configuration_ready)
            return `${saved} History configuration is not ready; runtime application is not confirmed.`;
        if (runtime.applied_revision === null)
            return `${saved} Runtime applied revision is unavailable; recording is not confirmed.`;
        if (runtime.applied_revision > settings.revision)
            return `${saved} Runtime reports newer applied revision ${runtime.applied_revision}; this settings readback is stale. Refresh to check the current settings.`;
        if (runtime.enabled !== settings.settings.enabled)
            return `${saved} Runtime reports a different recording policy from this saved readback; refresh to check the current settings.`;
        if (runtime.applied_revision < settings.revision) {
            const deferred = runtime.effective_enabled !== runtime.enabled &&
                (runtime.active_streams > 0 || runtime.pending_activities > 0);
            const policy = deferred ? ` ${Amaterasu.historyRecordingPolicyDescription(runtime)} ${Amaterasu.historyActivityDescription(runtime)}.` : "";
            return `${saved} Runtime status is stale: applied revision ${runtime.applied_revision} is behind; recording at saved revision ${settings.revision} is not confirmed.${policy}`;
        }
        const policy = Amaterasu.historyRecordingPolicyDescription(runtime);
        const activity = ` ${Amaterasu.historyActivityDescription(runtime)}.`;
        const retention = runtime.effective_enabled
            ? ""
            : " Stored sessions remain readable. Automatic retention and deletion are paused until recording is enabled.";
        return runtime.degraded
            ? `${saved} Runtime applied revision ${runtime.applied_revision} confirmed. ${policy}${activity}${retention} History runtime is degraded; check failed operations and retention status.`
            : `${saved} Runtime applied revision ${runtime.applied_revision} confirmed. ${policy}${activity}${retention}`;
    }
    Amaterasu.historySettingsRuntimeDescription = historySettingsRuntimeDescription;
    async function saveAuthTokenLifetimes(api, revision, sessionTokenTtlSeconds, rememberMeRefreshTokenTtlSeconds) {
        if (!Number.isSafeInteger(revision) || revision < 0)
            throw new Error("Authentication settings revision is invalid.");
        const values = parseAuthTokenLifetimes(sessionTokenTtlSeconds, rememberMeRefreshTokenTtlSeconds);
        const updated = parseAuthTokenSettingsReply(await api.post("auth/settings_update", {
            ...values,
            expected_revision: revision,
        }));
        if (updated.revision !== revision + 1 ||
            updated.values.session_token_ttl_seconds !== values.session_token_ttl_seconds ||
            updated.values.remember_me_refresh_token_ttl_seconds !== values.remember_me_refresh_token_ttl_seconds)
            throw new Error("Authentication settings update could not be confirmed. Refresh and check the current values.");
        const saved = parseAuthTokenSettingsReply(await api.post("auth/settings"));
        if (saved.revision !== updated.revision ||
            saved.values.session_token_ttl_seconds !== values.session_token_ttl_seconds ||
            saved.values.remember_me_refresh_token_ttl_seconds !== values.remember_me_refresh_token_ttl_seconds)
            throw new Error("Authentication settings could not be confirmed after saving. Refresh and check the current values.");
        return saved;
    }
    Amaterasu.saveAuthTokenLifetimes = saveAuthTokenLifetimes;
    function parseAuthTokenSettingsReply(value) {
        if (value === null || typeof value !== "object" || Array.isArray(value))
            throw new Error("Authentication settings reply is invalid.");
        const reply = value;
        if (reply.ok !== true || !Number.isSafeInteger(reply.revision) || reply.revision < 0 ||
            reply.values === null || typeof reply.values !== "object" || Array.isArray(reply.values))
            throw new Error("Authentication settings reply is invalid.");
        const values = reply.values;
        return {
            ok: true,
            revision: reply.revision,
            values: parseAuthTokenLifetimes(values.session_token_ttl_seconds, values.remember_me_refresh_token_ttl_seconds),
        };
    }
    Amaterasu.parseAuthTokenSettingsReply = parseAuthTokenSettingsReply;
    class AuthTokenSettingsLoadGate {
        generation = 0;
        invalidate() {
            this.generation += 1;
        }
        async read(api) {
            const generation = ++this.generation;
            try {
                const settings = parseAuthTokenSettingsReply(await api.post("auth/settings"));
                return generation === this.generation ? settings : null;
            }
            catch (error) {
                if (generation !== this.generation)
                    return null;
                throw error;
            }
        }
    }
    Amaterasu.AuthTokenSettingsLoadGate = AuthTokenSettingsLoadGate;
    function parseRegisteredConfigurationReply(value) {
        if (value === null || typeof value !== "object" || Array.isArray(value))
            throw new Error("Registered settings reply is invalid.");
        const reply = value;
        if (reply.ok !== true || !Number.isSafeInteger(reply.revision) || reply.revision < 0 ||
            reply.fields === null || typeof reply.fields !== "object" || Array.isArray(reply.fields) ||
            reply.values === null || typeof reply.values !== "object" || Array.isArray(reply.values))
            throw new Error("Registered settings reply is invalid.");
        return reply;
    }
    Amaterasu.parseRegisteredConfigurationReply = parseRegisteredConfigurationReply;
    function sameConfigurationValue(left, right) {
        if (left === right)
            return true;
        if (Array.isArray(left) && Array.isArray(right))
            return left.length === right.length && left.every((value, index) => sameConfigurationValue(value, right[index]));
        if (!left || !right || typeof left !== "object" || typeof right !== "object" ||
            Array.isArray(left) || Array.isArray(right))
            return false;
        const leftObject = left;
        const rightObject = right;
        const keys = Object.keys(leftObject);
        return keys.length === Object.keys(rightObject).length &&
            keys.every((key) => Object.hasOwn(rightObject, key) && sameConfigurationValue(leftObject[key], rightObject[key]));
    }
    async function saveRegisteredConfiguration(api, current, values) {
        const expectedValues = { ...current.values, ...values };
        const confirms = (reply) => reply.revision === current.revision + 1 &&
            sameConfigurationValue(reply.fields, current.fields) &&
            sameConfigurationValue(reply.values, expectedValues);
        const updated = parseRegisteredConfigurationReply(await api.post("configuration/update", {
            values,
            expected_revision: current.revision,
        }));
        if (!confirms(updated))
            throw new Error("Registered settings update could not be confirmed. Refresh and check the current values.");
        const saved = parseRegisteredConfigurationReply(await api.post("configuration/get"));
        if (!confirms(saved))
            throw new Error("Registered settings could not be confirmed after saving. Refresh and check the current values.");
        return saved;
    }
    Amaterasu.saveRegisteredConfiguration = saveRegisteredConfiguration;
    class RegisteredConfigurationLoadGate {
        generation = 0;
        invalidate() {
            this.generation += 1;
        }
        async read(api) {
            const generation = ++this.generation;
            try {
                const settings = parseRegisteredConfigurationReply(await api.post("configuration/get"));
                return generation === this.generation ? settings : null;
            }
            catch (error) {
                if (generation !== this.generation)
                    return null;
                throw error;
            }
        }
    }
    Amaterasu.RegisteredConfigurationLoadGate = RegisteredConfigurationLoadGate;
    function parseNvhttpSettingsReply(value) {
        if (value === null || typeof value !== "object" || Array.isArray(value))
            throw new Error("Moonlight host settings reply is invalid.");
        const reply = value;
        if (reply.ok !== true || !Number.isSafeInteger(reply.revision) || reply.revision < 0 ||
            typeof reply.configured_hostname !== "string" ||
            !Number.isSafeInteger(reply.port) ||
            !Array.isArray(reply.configured_global_preparation))
            throw new Error("Moonlight host settings reply is invalid.");
        return reply;
    }
    Amaterasu.parseNvhttpSettingsReply = parseNvhttpSettingsReply;
    function normalizedPreparationSteps(value) {
        if (!Array.isArray(value))
            return value;
        const command = (raw) => ({
            executable: raw.executable,
            args: raw.args ?? [],
            environment: raw.environment ?? {},
            cwd: raw.cwd ?? null,
            timeout_ms: raw.timeout_ms ?? 10_000,
        });
        return value.map((step) => ({
            run: command(step.run),
            undo: step.undo ? command(step.undo) : null,
        }));
    }
    async function saveNvhttpHostSettings(api, current, values) {
        const preserved = ["port", "stream_audio", "audio_sink_capture_only", "nvenc_spatial_aq",
            "lan_encryption_mode", "wan_encryption_mode"];
        const expected = {};
        for (const [key, value] of Object.entries(current))
            if (key.startsWith("configured_") || preserved.includes(key))
                expected[key] = value;
        Object.assign(expected, {
            configured_hostname: values.hostname,
            port: values.port,
            configured_address_family: values.address_family,
            configured_bind_address: values.bind_address,
            configured_capture: values.capture,
            configured_output_name: values.output_name === "" ? null : values.output_name,
            stream_audio: values.stream_audio,
            configured_video_max_batch_size_kb: values.video_max_batch_size_kb,
            configured_max_bitrate: values.max_bitrate,
            configured_minimum_fps_target: values.minimum_fps_target,
            configured_ping_timeout_ms: values.ping_timeout_ms === -1 ? null : values.ping_timeout_ms,
            configured_audio_sink: values.audio_sink,
            configured_virtual_sink: values.virtual_sink,
            audio_sink_capture_only: values.audio_sink_capture_only,
            configured_external_ip: values.external_ip,
            configured_wgc_pacing_smoothing: values.wgc_pacing_smoothing,
            configured_global_preparation: normalizedPreparationSteps(values.global_preparation),
        });
        const confirms = (reply) => reply.revision === current.revision + 1 &&
            Object.entries(expected).every(([key, value]) => sameConfigurationValue(reply[key], value));
        const updated = parseNvhttpSettingsReply(await api.post("nvhttp/settings_update", {
            ...values,
            expected_revision: current.revision,
        }));
        if (!confirms(updated))
            throw new Error("Moonlight host update could not be confirmed. Refresh and check the current values.");
        const saved = parseNvhttpSettingsReply(await api.post("nvhttp/settings"));
        if (!confirms(saved))
            throw new Error("Moonlight host settings could not be confirmed after saving. Refresh and check the current values.");
        return saved;
    }
    Amaterasu.saveNvhttpHostSettings = saveNvhttpHostSettings;
    class NvhttpSettingsLoadGate {
        generation = 0;
        invalidate() {
            this.generation += 1;
        }
        isCurrent(generation) {
            return generation === this.generation;
        }
        async read(api) {
            const generation = ++this.generation;
            try {
                const settings = parseNvhttpSettingsReply(await api.post("nvhttp/settings"));
                return this.isCurrent(generation) ? { settings, generation } : null;
            }
            catch (error) {
                if (!this.isCurrent(generation))
                    return null;
                throw error;
            }
        }
    }
    Amaterasu.NvhttpSettingsLoadGate = NvhttpSettingsLoadGate;
    async function recoverNvhttpSaveFailure(code, reload, notify) {
        if (code !== "revision_conflict" &&
            !["listener_apply_pending", "listener_apply_failed", "listener_not_active"].includes(code))
            return false;
        await reload();
        notify(code === "revision_conflict"
            ? "Host settings changed elsewhere. Review the latest values before saving."
            : "Host settings were stored, but listener application is not confirmed. Review the active ports and listener error.");
        return true;
    }
    Amaterasu.recoverNvhttpSaveFailure = recoverNvhttpSaveFailure;
    const hostLogLevels = [
        "product_default", "verbose", "debug", "info", "warning", "error", "fatal", "none",
    ];
    function parseHostLoggingSettings(level, path) {
        if (!hostLogLevels.includes(level))
            throw new Error("Choose a supported logging level.");
        const pathComponents = path.split(/[\\/]/u).filter(Boolean);
        if (!path ||
            new TextEncoder().encode(path).length > 4096 ||
            /[\u0000-\u001f\u007f-\u009f]/u.test(path) ||
            path.split(/[\\/]/u).includes("..") ||
            (pathComponents.length > 0 && pathComponents.every((component) => component === ".")) ||
            /^[a-zA-Z]:(?![\\/])/u.test(path))
            throw new Error("Enter a log path of at most 4096 UTF-8 bytes without controls or parent traversal.");
        return { min_log_level: level, log_path: path };
    }
    Amaterasu.parseHostLoggingSettings = parseHostLoggingSettings;
    function parseHostConfigurationReply(reply) {
        if (!reply || typeof reply !== "object" || Array.isArray(reply))
            throw new Error("Host configuration reply is invalid.");
        const document = reply;
        if (document.ok !== true || document.ready !== true || document.version !== 1 ||
            !Number.isSafeInteger(document.revision) || document.revision < 0)
            throw new Error("Host configuration version or revision is invalid.");
        if (!document.values || typeof document.values !== "object" || Array.isArray(document.values) ||
            !document.effective || typeof document.effective !== "object" || Array.isArray(document.effective))
            throw new Error("Host configuration values are invalid.");
        const values = document.values;
        const effective = document.effective;
        for (const key of ["file_state", "vibeshine_file_state", "credentials_file", "pkey", "cert", "log_path"])
            if (typeof values[key] !== "string")
                throw new Error(`Host configuration ${key} is invalid.`);
        if (typeof effective.log_path !== "string" ||
            !hostLogLevels.includes(effective.min_log_level) ||
            !hostLogLevels.includes(values.min_log_level))
            throw new Error("Effective host logging settings are invalid.");
        if (Object.hasOwn(document, "logging")) {
            const logging = document.logging;
            if (!logging || typeof logging !== "object" || Array.isArray(logging))
                throw new Error("Host logging application confirmation is invalid.");
            const confirmation = logging;
            if (Object.keys(confirmation).sort().join(",") !== "applied_revision,saved_revision,status" ||
                confirmation.saved_revision !== document.revision ||
                confirmation.applied_revision !== document.revision ||
                confirmation.status !== "applied")
                throw new Error("Host logging application confirmation is invalid.");
        }
        return document;
    }
    Amaterasu.parseHostConfigurationReply = parseHostConfigurationReply;
    async function saveHostLoggingSettings(api, current, level, path) {
        const logging = parseHostLoggingSettings(level, path);
        const updated = parseHostConfigurationReply(await api.post("host-configuration/update", {
            ...current.values,
            ...logging,
            expected_revision: current.revision,
        }));
        if (!confirmsHostLoggingUpdate(updated, current, logging) || !updated.logging)
            throw new Error("Host logging update could not be confirmed. Refresh and check the current values.");
        const saved = parseHostConfigurationReply(await api.post("host-configuration/get"));
        if (!confirmsHostLoggingUpdate(saved, current, logging))
            throw new Error("Host logging settings could not be confirmed after saving. Refresh and check the current values.");
        return { ...saved, logging: updated.logging };
    }
    Amaterasu.saveHostLoggingSettings = saveHostLoggingSettings;
    function confirmsHostLoggingUpdate(reply, current, logging) {
        return reply.revision === current.revision + 1 &&
            reply.values.log_path === logging.log_path &&
            reply.values.min_log_level === logging.min_log_level &&
            ["file_state", "vibeshine_file_state", "credentials_file", "pkey", "cert"].every((key) => reply.values[key] === current.values[key]);
    }
    function parseDiagnosticsLoggingStatus(value) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Diagnostics status is invalid.");
        const status = value;
        const revision = status.logging_configuration_revision;
        if (status.ok !== true || typeof status.ready !== "boolean" ||
            (revision !== null && (!Number.isSafeInteger(revision) || revision < 1)))
            throw new Error("Diagnostics logging status is invalid.");
        if (Object.hasOwn(status, "logging_sink")) {
            const sink = status.logging_sink;
            if (!sink || typeof sink !== "object" || Array.isArray(sink))
                throw new Error("Diagnostics logging sink status is invalid.");
            const { state, failure } = sink;
            if ((state !== "disabled" && state !== "healthy" && state !== "failed") ||
                (state === "failed" ? failure !== "open" && failure !== "write" : failure !== null))
                throw new Error("Diagnostics logging sink status is invalid.");
        }
        return status;
    }
    Amaterasu.parseDiagnosticsLoggingStatus = parseDiagnosticsLoggingStatus;
    async function readHostLoggingState(api) {
        const settings = parseHostConfigurationReply(await api.post("host-configuration/get"));
        return { settings, diagnostics: await readDiagnosticsLoggingStatus(api) };
    }
    Amaterasu.readHostLoggingState = readHostLoggingState;
    class HostLoggingLoadGate {
        generation = 0;
        invalidate() {
            this.generation += 1;
        }
        async read(api) {
            const generation = ++this.generation;
            try {
                const state = await readHostLoggingState(api);
                return generation === this.generation ? state : null;
            }
            catch (error) {
                if (generation !== this.generation)
                    return null;
                throw error;
            }
        }
    }
    Amaterasu.HostLoggingLoadGate = HostLoggingLoadGate;
    async function readDiagnosticsLoggingStatus(api) {
        if (api.allows && !api.allows("diagnostics.read"))
            return { status: null, error: "permission" };
        let reply;
        try {
            reply = await api.post("diagnostics/status");
        }
        catch {
            return { status: null, error: "request" };
        }
        try {
            return { status: parseDiagnosticsLoggingStatus(reply), error: null };
        }
        catch {
            return { status: null, error: "invalid" };
        }
    }
    async function saveHostLoggingState(api, current, level, path) {
        const settings = await saveHostLoggingSettings(api, current, level, path);
        return { settings, diagnostics: await readDiagnosticsLoggingStatus(api) };
    }
    Amaterasu.saveHostLoggingState = saveHostLoggingState;
    function hostLoggingSaveFailureNotice(code) {
        switch (code) {
            case "logging_apply_failed":
                return "Host logging application failed and the previous settings were restored. Review the refreshed policy before saving again.";
            case "save_unconfirmed":
                return "Host logging write outcome is unknown. Review the refreshed policy and diagnostics status before retrying.";
            case "logging_application_unconfirmed":
                return "Host logging settings may be saved, but application is unconfirmed. Review the refreshed policy and diagnostics status before retrying.";
            case "logging_rollback_failed":
                return "Host logging application failed and previous settings could not be restored. Configuration may differ from the active policy; review both statuses before retrying.";
            case "logging_rollback_unconfirmed":
                return "Host logging application failed and restoration is unconfirmed. Configuration may differ from the active policy; review both statuses before retrying.";
            default:
                return "Host logging save could not be confirmed. Review the refreshed policy before saving again.";
        }
    }
    Amaterasu.hostLoggingSaveFailureNotice = hostLoggingSaveFailureNotice;
    function diagnosticsLoggingDescription(savedRevision, result) {
        if (result.error === "permission")
            return "Saved host logging policy; diagnostics status requires diagnostics.read permission.";
        if (result.error === "request")
            return "Saved host logging policy; diagnostics status request failed. Refresh to retry.";
        if (result.error === "invalid")
            return "Saved host logging policy; diagnostics returned invalid status. Refresh to retry.";
        const status = result.status;
        if (!status)
            return "Saved host logging policy; diagnostics status is unavailable. Refresh to check whether it applied.";
        if (!status.ready)
            return "Saved host logging policy; diagnostics is not ready. Refresh after it starts.";
        if (status.logging_configuration_revision === savedRevision) {
            if (!status.logging_sink)
                return `Diagnostics is using saved host logging revision ${savedRevision}; disk delivery is unverified.`;
            if (status.logging_sink.state === "disabled")
                return `Diagnostics is using saved host logging revision ${savedRevision}; disk logging is disabled.`;
            if (status.logging_sink.state === "failed")
                return status.logging_sink.failure === "open"
                    ? `Diagnostics is using saved host logging revision ${savedRevision}, but opening the disk log failed. Disk delivery is not working.`
                    : `Diagnostics is using saved host logging revision ${savedRevision}, but writing the disk log failed. Disk delivery is not working.`;
            return `Diagnostics is using saved host logging revision ${savedRevision}; disk logging is healthy.`;
        }
        if (status.logging_configuration_revision === null || status.logging_configuration_revision < savedRevision)
            return `Saved host logging revision ${savedRevision} is pending in diagnostics. Refresh to check again.`;
        return `Diagnostics is using a newer host logging revision ${status.logging_configuration_revision}. Refresh the saved policy.`;
    }
    Amaterasu.diagnosticsLoggingDescription = diagnosticsLoggingDescription;
    function parseInputPolicy(keyboard, mouse, highResolutionScrolling, alwaysSendScancodes, keyRightAltToKeyWin, keyRepeatDelay, keyRepeatFrequency, backButtonTimeout = -1, gamepadProfile = "auto", selectableGamepadProfiles = ["auto"], controllerEnabled = true, nativePenTouch = true) {
        if (typeof keyboard !== "boolean" ||
            typeof mouse !== "boolean" ||
            typeof highResolutionScrolling !== "boolean" ||
            typeof alwaysSendScancodes !== "boolean" ||
            typeof keyRightAltToKeyWin !== "boolean" ||
            !Number.isSafeInteger(keyRepeatDelay) ||
            keyRepeatDelay < 0 ||
            keyRepeatDelay > maxInputKeyRepeatDelayMs ||
            !Number.isFinite(keyRepeatFrequency) ||
            keyRepeatFrequency < 0 ||
            !Number.isSafeInteger(backButtonTimeout) ||
            backButtonTimeout < minInputBackButtonTimeoutMs ||
            backButtonTimeout > maxInputBackButtonTimeoutMs ||
            typeof gamepadProfile !== "string" ||
            !Array.isArray(selectableGamepadProfiles) ||
            !selectableGamepadProfiles.includes(gamepadProfile) ||
            typeof controllerEnabled !== "boolean" ||
            typeof nativePenTouch !== "boolean")
            throw new Error("Input policy values, key repeat settings, or Back timeout are invalid.");
        return {
            keyboard,
            mouse,
            high_resolution_scrolling: highResolutionScrolling,
            always_send_scancodes: alwaysSendScancodes,
            key_rightalt_to_key_win: keyRightAltToKeyWin,
            key_repeat_delay: keyRepeatDelay,
            key_repeat_frequency: keyRepeatFrequency,
            back_button_timeout: backButtonTimeout,
            gamepad: gamepadProfile,
            controller: controllerEnabled,
            native_pen_touch: nativePenTouch,
        };
    }
    Amaterasu.parseInputPolicy = parseInputPolicy;
    const recognizedInputGamepadProfiles = [
        "auto", "x360", "vhf", "vhf_xbox", "vhf_xbox_one", "vhf_ds4", "vhf_ds5", "vhf_switch",
        "ds4", "xone", "ds5", "switch",
    ];
    function parseInputPolicySettingsReply(value) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Input policy settings reply is invalid.");
        const reply = value;
        if (reply.ok !== true || reply.persisted !== true ||
            typeof reply.applied !== "boolean" || typeof reply.cleanup_required !== "boolean" ||
            !Number.isSafeInteger(reply.revision) || reply.revision < 0 ||
            !reply.values || typeof reply.values !== "object" || Array.isArray(reply.values))
            throw new Error("Input policy settings reply is invalid.");
        const values = reply.values;
        if (typeof values.ds4_back_as_touchpad_click !== "boolean")
            throw new Error("Input policy settings reply omitted the DS4 back button policy.");
        const parsed = {
            ...parseInputPolicy(values.keyboard, values.mouse, values.high_resolution_scrolling, values.always_send_scancodes, values.key_rightalt_to_key_win, values.key_repeat_delay, values.key_repeat_frequency, values.back_button_timeout, values.gamepad, recognizedInputGamepadProfiles, values.controller, values.native_pen_touch),
            ds4_back_as_touchpad_click: values.ds4_back_as_touchpad_click,
        };
        if (!sameConfigurationValue(values, parsed))
            throw new Error("Input policy settings reply contains unexpected values.");
        return {
            ok: true,
            persisted: true,
            applied: reply.applied,
            cleanup_required: reply.cleanup_required,
            revision: reply.revision,
            values: parsed,
        };
    }
    Amaterasu.parseInputPolicySettingsReply = parseInputPolicySettingsReply;
    async function saveInputPolicySettings(api, current, values) {
        if (!Number.isSafeInteger(current.revision) || current.revision < 0 ||
            current.revision >= Number.MAX_SAFE_INTEGER)
            throw new Error("Input policy settings revision is invalid.");
        const confirms = (reply) => reply.revision === current.revision + 1 && sameConfigurationValue(reply.values, values);
        try {
            const updated = parseInputPolicySettingsReply(await api.post("input/settings_update", {
                ...values,
                expected_revision: current.revision,
            }));
            if (!confirms(updated))
                throw new Error("Input policy update could not be confirmed. Refresh and check the current values.");
        }
        catch (error) {
            if (!(error instanceof Amaterasu.ApiError) || error.code !== "cleanup_pending")
                throw error;
        }
        const saved = parseInputPolicySettingsReply(await api.post("input/settings_get"));
        if (!confirms(saved))
            throw new Error("Input policy settings could not be confirmed after saving. Refresh and check the current values.");
        return saved;
    }
    Amaterasu.saveInputPolicySettings = saveInputPolicySettings;
    function parseInputRuntimeStatus(value) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Input runtime status is invalid.");
        const status = value;
        if (status.ok !== true || typeof status.gamepad !== "string" ||
            !Array.isArray(status.supported_profiles) ||
            !status.supported_profiles.every((profile) => typeof profile === "string") ||
            !Array.isArray(status.recognized_profiles) ||
            !status.recognized_profiles.every((profile) => typeof profile === "string") ||
            typeof status.controller_input !== "boolean" ||
            typeof status.controller_enabled !== "boolean" ||
            typeof status.controller_cleanup_required !== "boolean" ||
            (status.lease !== null && (!status.lease || typeof status.lease !== "object" || Array.isArray(status.lease))) ||
            (status.gamepad_unavailable_reason !== null && typeof status.gamepad_unavailable_reason !== "string"))
            throw new Error("Input runtime status is invalid.");
        return status;
    }
    class InputPolicyLoadGate {
        generation = 0;
        invalidate() {
            this.generation += 1;
        }
        async read(api) {
            const generation = ++this.generation;
            try {
                const settings = parseInputPolicySettingsReply(await api.post("input/settings_get"));
                if (generation !== this.generation)
                    return null;
                let runtime = null;
                try {
                    runtime = parseInputRuntimeStatus(await api.post("input/status"));
                }
                catch {
                }
                return generation === this.generation ? { settings, runtime } : null;
            }
            catch (error) {
                if (generation !== this.generation)
                    return null;
                throw error;
            }
        }
    }
    Amaterasu.InputPolicyLoadGate = InputPolicyLoadGate;
    function inputGamepadProfileChoices(selectedProfile, supportedProfiles, recognizedProfiles = supportedProfiles) {
        if (typeof selectedProfile !== "string" ||
            !Array.isArray(supportedProfiles) ||
            !supportedProfiles.every((profile) => typeof profile === "string") ||
            !Array.isArray(recognizedProfiles) ||
            !recognizedProfiles.every((profile) => typeof profile === "string"))
            throw new Error("Gamepad profile status is invalid.");
        const availableProfiles = [...new Set(supportedProfiles)];
        if (availableProfiles.length === 0)
            availableProfiles.push("auto");
        const choices = [...new Set([...recognizedProfiles, ...availableProfiles])].map((value) => ({
            value,
            label: availableProfiles.includes(value)
                ? value
                : `${value} (recognized, unavailable on this backend)`,
            available: availableProfiles.includes(value),
        }));
        if (selectedProfile !== "" && !choices.some((choice) => choice.value === selectedProfile)) {
            choices.push({
                value: selectedProfile,
                label: `${selectedProfile} (recognized, unavailable on this backend)`,
                available: false,
            });
        }
        return choices;
    }
    Amaterasu.inputGamepadProfileChoices = inputGamepadProfileChoices;
    function inputGamepadStatusDescription(selectedProfile, status) {
        if (!status || !Array.isArray(status.supported_profiles))
            return "Current gamepad backend status is unavailable.";
        if (!status.controller_enabled)
            return status.controller_cleanup_required
                ? "Controller input is disabled; virtual gamepad removal is retrying. Keyboard and mouse input remain available."
                : "Controller input is disabled by policy. Keyboard and mouse input remain available.";
        const isSupported = status.supported_profiles.includes(selectedProfile);
        if (!isSupported) {
            const reason = status.gamepad === selectedProfile &&
                typeof status.gamepad_unavailable_reason === "string" &&
                status.gamepad_unavailable_reason !== ""
                ? status.gamepad_unavailable_reason
                : null;
            return reason
                ? `The selected ${selectedProfile} profile is recognized but unavailable: ${reason}`
                : `The selected ${selectedProfile} profile is recognized but unavailable on this backend.`;
        }
        if (status.lease === null)
            return `${selectedProfile} is supported by this backend; device availability is checked when a stream starts.`;
        const reason = status.gamepad === selectedProfile &&
            typeof status.gamepad_unavailable_reason === "string" &&
            status.gamepad_unavailable_reason !== ""
            ? status.gamepad_unavailable_reason
            : null;
        if (status.gamepad === selectedProfile && !status.controller_input)
            return reason
                ? `The ${selectedProfile} profile is selected, but controller input is currently unavailable: ${reason}`
                : `The ${selectedProfile} profile is selected, but controller input is currently unavailable.`;
        return `${selectedProfile} is supported by the current input backend.`;
    }
    Amaterasu.inputGamepadStatusDescription = inputGamepadStatusDescription;
    function parseClientFecOverride(value) {
        const trimmed = value.trim();
        if (trimmed === "")
            return { fec_percentage: null };
        if (!/^(?:[1-9][0-9]{0,2})$/.test(trimmed))
            throw new Error("FEC percentage must be a whole number from 1 to 255, or blank to inherit.");
        const percentage = Number(trimmed);
        if (percentage > 255)
            throw new Error("FEC percentage must be a whole number from 1 to 255, or blank to inherit.");
        return { fec_percentage: percentage };
    }
    Amaterasu.parseClientFecOverride = parseClientFecOverride;
    const forwardablePairingOverrideFields = [
        "fec_percentage", "stream_audio", "audio_sink", "virtual_sink",
        "audio_sink_capture_only", "back_button_timeout", "controller", "gamepad",
        "keyboard", "mouse", "native_pen_touch",
        "max_bitrate", "minimum_fps_target", "video_max_batch_size_kb",
        "frame_limiter_fps_limit", "frame_limiter_enable", "frame_limiter_provider",
        "frame_limiter_disable_vsync", "frame_limiter_auto_virtual_framegen",
        "rtss_frame_limit_type", "nvenc_spatial_aq", "high_resolution_scrolling",
        "always_send_scancodes", "key_rightalt_to_key_win", "key_repeat_delay",
        "key_repeat_frequency",
    ];
    function unsupportedPairingOverrideFields(client) {
        const overrides = client.config_overrides ?? {};
        return Object.entries(overrides)
            .filter(([field, value]) => !forwardablePairingOverrideFields.includes(field) ||
            !validStoredPairingOverride(field, value))
            .map(([field]) => field);
    }
    Amaterasu.unsupportedPairingOverrideFields = unsupportedPairingOverrideFields;
    function validStoredPairingOverride(field, value) {
        if (value === null || value === undefined)
            return false;
        if (["stream_audio", "audio_sink_capture_only", "controller", "keyboard", "mouse",
            "native_pen_touch", "frame_limiter_enable", "frame_limiter_disable_vsync",
            "nvenc_spatial_aq", "high_resolution_scrolling", "always_send_scancodes",
            "key_rightalt_to_key_win"].includes(field))
            return typeof value === "boolean";
        if (field === "audio_sink" || field === "virtual_sink")
            return typeof value === "string" && new TextEncoder().encode(value).length <= 4096 &&
                !/[\u0000-\u001f\u007f-\u009f]/.test(value);
        if (field === "gamepad")
            return typeof value === "string" && recognizedInputGamepadProfiles.includes(value);
        if (field === "frame_limiter_provider")
            return typeof value === "string" && ["auto", "rtss", "nvidia-control-panel", "none",
                "mangohud", "proton", "mangohud-proton"].includes(value);
        if (field === "frame_limiter_auto_virtual_framegen")
            return typeof value === "string" && ["enabled", "vrr", "legacy", "disabled"].includes(value);
        if (field === "rtss_frame_limit_type")
            return typeof value === "string" && ["async", "front edge sync", "back edge sync",
                "nvidia reflex"].includes(value);
        if (field === "fec_percentage")
            return Number.isSafeInteger(value) && value >= 1 && value <= 255;
        if (field === "video_max_batch_size_kb")
            return Number.isSafeInteger(value) && value >= 0 && value <= 64;
        if (field === "back_button_timeout" || field === "max_bitrate")
            return Number.isSafeInteger(value) && value >= -0x8000_0000 &&
                value <= 0x7fff_ffff;
        if (field === "key_repeat_delay")
            return Number.isSafeInteger(value) && value >= 0 && value <= 0x7fff_ffff;
        if (field === "frame_limiter_fps_limit")
            return typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 1000 &&
                Math.abs(value * 1000 - Math.round(value * 1000)) <= 0.000001;
        if (["minimum_fps_target", "key_repeat_frequency"].includes(field))
            return typeof value === "number" && Number.isFinite(value) && value >= 0 &&
                (field === "key_repeat_frequency" || value <= 1000);
        return false;
    }
    function parsePairingClientsReply(value) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Paired clients reply is invalid.");
        const reply = value;
        if (reply.ok !== true || !Number.isSafeInteger(reply.revision) ||
            reply.revision < 0 || !Array.isArray(reply.clients))
            throw new Error("Paired clients reply is invalid.");
        const seen = new Set();
        for (const value of reply.clients) {
            if (!value || typeof value !== "object" || Array.isArray(value))
                throw new Error("Paired client metadata is invalid.");
            const client = value;
            if (typeof client.certificate_id !== "string" ||
                !/^[0-9a-fA-F]{64}$/.test(client.certificate_id) ||
                seen.has(client.certificate_id) ||
                typeof client.client_id !== "string" || typeof client.name !== "string" ||
                !Number.isSafeInteger(client.paired_at) || client.paired_at < 0 ||
                (client.config_overrides !== undefined &&
                    (!client.config_overrides || typeof client.config_overrides !== "object" ||
                        Array.isArray(client.config_overrides))))
                throw new Error("Paired client metadata is invalid.");
            seen.add(client.certificate_id);
        }
        return reply;
    }
    Amaterasu.parsePairingClientsReply = parsePairingClientsReply;
    function canonicalPairingOverrides(values) {
        if (Object.keys(values).some((field) => !forwardablePairingOverrideFields.includes(field)))
            throw new Error("The paired client has override fields this editor cannot preserve.");
        return Object.fromEntries(Object.entries(values).filter(([, value]) => value !== null && value !== undefined));
    }
    async function savePairingClientOverrides(api, current, certificateId, values) {
        const client = current.clients.find((candidate) => candidate.certificate_id === certificateId);
        if (!client || !Number.isSafeInteger(current.revision) || current.revision >= Number.MAX_SAFE_INTEGER)
            throw new Error("Paired client or settings revision is invalid.");
        const unsupported = unsupportedPairingOverrideFields(client);
        if (unsupported.length > 0)
            throw new Error(`This client has overrides this editor cannot preserve: ${unsupported.join(", ")}.`);
        const desired = canonicalPairingOverrides({ ...client.config_overrides, ...values });
        const confirms = (reply) => {
            if (reply.revision !== current.revision + 1 || reply.clients.length !== current.clients.length)
                return false;
            return current.clients.every((before) => {
                const after = reply.clients.find((candidate) => candidate.certificate_id === before.certificate_id);
                if (!after || after.client_id !== before.client_id || after.name !== before.name ||
                    after.paired_at !== before.paired_at)
                    return false;
                return sameConfigurationValue(after.config_overrides ?? {}, before.certificate_id === certificateId ? desired : before.config_overrides ?? {});
            });
        };
        const response = await api.post("pairing/update_client_config_overrides", {
            certificate_id: certificateId,
            expected_revision: current.revision,
            config_overrides: desired,
        });
        if (!response || typeof response !== "object" || Array.isArray(response) ||
            response.ok !== true ||
            response.certificate_id !== certificateId)
            throw new Error("Paired client update reply did not confirm the selected certificate.");
        const saved = parsePairingClientsReply(await api.post("pairing/list_clients"));
        if (!confirms(saved))
            throw new Error("Paired client overrides could not be confirmed after saving. Refresh and check current values.");
        return saved;
    }
    Amaterasu.savePairingClientOverrides = savePairingClientOverrides;
    class PairingClientsLoadGate {
        generation = 0;
        invalidate() {
            this.generation += 1;
        }
        async read(api) {
            const generation = ++this.generation;
            try {
                const settings = parsePairingClientsReply(await api.post("pairing/list_clients"));
                if (generation !== this.generation)
                    return null;
                let inputStatus = null;
                try {
                    inputStatus = parseInputRuntimeStatus(await api.post("input/status"));
                }
                catch {
                }
                return generation === this.generation ? { settings, inputStatus } : null;
            }
            catch (error) {
                if (generation !== this.generation)
                    return null;
                throw error;
            }
        }
    }
    Amaterasu.PairingClientsLoadGate = PairingClientsLoadGate;
    function sparseInputOverrideSelect(name, current) {
        const select = Amaterasu.el("select");
        select.name = name;
        select.setAttribute("aria-label", `${name} override`);
        const choices = [["", "Inherit"], ["true", "Enabled"], ["false", "Disabled"]];
        for (const [value, label] of choices) {
            const option = Amaterasu.el("option", "", label);
            option.value = value;
            select.append(option);
        }
        select.value = current === undefined ? "" : String(current);
        return select;
    }
    function inputPolicyStatusDescription(settings) {
        if (!settings.persisted)
            return "The saved input policy has not been confirmed yet.";
        if (!settings.applied || settings.cleanup_required)
            return "The saved policy is active for new events, but cleanup of held native input is still pending.";
        return "The saved keyboard, mouse, and gamepad policy is active.";
    }
    Amaterasu.inputPolicyStatusDescription = inputPolicyStatusDescription;
    function inputPolicyRuntimeDescription(settings, runtime) {
        if (runtime === null)
            return "Saved input policy confirmed. Runtime status is unavailable; application is not confirmed.";
        if (runtime.gamepad !== settings.values.gamepad ||
            runtime.controller_enabled !== settings.values.controller)
            return "Saved input policy confirmed, but runtime has not reported the selected gamepad and controller policy.";
        if (runtime.controller_cleanup_required || settings.cleanup_required || !settings.applied)
            return "Saved input policy confirmed; releasing held native input is still pending.";
        return inputPolicyStatusDescription(settings);
    }
    Amaterasu.inputPolicyRuntimeDescription = inputPolicyRuntimeDescription;
    function safeNvhttpBindFamily(value) {
        if (value === "")
            return null;
        const octets = value.split(".");
        if (octets.length === 4) {
            const parsed = octets.map((octet) => Number(octet));
            if (octets.every((octet, index) => {
                const number = parsed[index];
                return (number !== undefined &&
                    /^(0|[1-9][0-9]{0,2})$/.test(octet) &&
                    number <= 255 &&
                    String(number) === octet);
            })) {
                const firstOctet = parsed[0];
                if (firstOctet !== undefined && firstOctet >= 224 && firstOctet <= 239)
                    throw new Error("NVHTTP bind address must not be multicast or broadcast.");
                if (value === "255.255.255.255")
                    throw new Error("NVHTTP bind address must not be multicast or broadcast.");
                return "ipv4";
            }
        }
        if (!value.includes(":") || /[%\[\]\s]/.test(value))
            return null;
        try {
            const hostname = new URL(`http://[${value}]/`).hostname.toLowerCase();
            if (!hostname.startsWith("[") || !hostname.endsWith("]"))
                return null;
            if (hostname.startsWith("[ff") || hostname.startsWith("[::ffff:"))
                throw new Error("NVHTTP bind address must not be multicast or IPv4-mapped IPv6.");
            return "ipv6";
        }
        catch (error) {
            if (error instanceof Error && error.message.startsWith("NVHTTP bind address"))
                throw error;
            return null;
        }
    }
    function parseNvhttpNetworkOverrides(addressFamilySelection, useStartupBindAddress, bindAddress) {
        const addressFamily = addressFamilySelection === "startup"
            ? null
            : addressFamilySelection === "ipv4" || addressFamilySelection === "both"
                ? addressFamilySelection
                : (() => {
                    throw new Error("Choose IPv4, both address families, or startup policy.");
                })();
        let configuredBindAddress = null;
        if (!useStartupBindAddress) {
            const bindFamily = safeNvhttpBindFamily(bindAddress);
            if (bindAddress !== "" && bindFamily === null)
                throw new Error("Enter a valid IPv4 or IPv6 address literal, or leave it blank.");
            if (addressFamily === "ipv4" && bindFamily === "ipv6")
                throw new Error("An IPv6 bind address requires both address families.");
            configuredBindAddress = bindAddress;
        }
        return {
            address_family: addressFamily,
            bind_address: configuredBindAddress,
        };
    }
    Amaterasu.parseNvhttpNetworkOverrides = parseNvhttpNetworkOverrides;
    function parseOutputNameSelection(value) {
        if (new TextEncoder().encode(value).length > 512 ||
            value.trim() !== value ||
            /[\u0000-\u001f\u007f-\u009f]/.test(value))
            throw new Error("The capture output ID must have at most 512 UTF-8 bytes without surrounding whitespace or control characters.");
        return value;
    }
    Amaterasu.parseOutputNameSelection = parseOutputNameSelection;
    function parseCaptureSelection(value) {
        if (value === "auto" || value === "ddx" || value === "wgc" || value === "wgcc")
            return value;
        throw new Error("Choose Automatic, Desktop Duplication, or a Windows Graphics Capture mode.");
    }
    Amaterasu.parseCaptureSelection = parseCaptureSelection;
    function parseVideoMaxBatchSize(value) {
        if (value.trim() === "")
            return null;
        if (!/^(0|[1-9][0-9]*)$/.test(value))
            throw new Error("Video batch size must be a whole number from 0 to 64 KiB.");
        const size = Number(value);
        if (!Number.isSafeInteger(size) || size > 64)
            throw new Error("Video batch size must be a whole number from 0 to 64 KiB.");
        return size;
    }
    Amaterasu.parseVideoMaxBatchSize = parseVideoMaxBatchSize;
    function parseMaxBitrate(value) {
        if (value.trim() === "")
            return null;
        if (!/^(0|-?[1-9][0-9]*)$/.test(value))
            throw new Error("Maximum bitrate must be a signed 32-bit whole number in kbps.");
        const bitrate = Number(value);
        if (!Number.isInteger(bitrate) || bitrate < -2_147_483_648 || bitrate > 2_147_483_647)
            throw new Error("Maximum bitrate must be a signed 32-bit whole number in kbps.");
        return bitrate;
    }
    Amaterasu.parseMaxBitrate = parseMaxBitrate;
    function parseMinimumFpsTarget(value) {
        if (value.trim() === "")
            return null;
        if (!/^(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(value))
            throw new Error("Minimum frame rate must be a finite number from 0 to 1000, or blank to use 20.");
        const target = Number(value);
        if (!Number.isFinite(target) || target < 0 || target > 1000)
            throw new Error("Minimum frame rate must be a finite number from 0 to 1000, or blank to use 20.");
        return target;
    }
    Amaterasu.parseMinimumFpsTarget = parseMinimumFpsTarget;
    function parsePingTimeout(value) {
        if (value.trim() === "")
            return -1;
        if (!/^(0|[1-9][0-9]*)$/.test(value))
            throw new Error("Ping timeout must be a whole number from 0 to 2147483647 milliseconds, or blank to reset.");
        const timeout = Number(value);
        if (!Number.isSafeInteger(timeout) || timeout > 2_147_483_647)
            throw new Error("Ping timeout must be a whole number from 0 to 2147483647 milliseconds, or blank to reset.");
        return timeout;
    }
    Amaterasu.parsePingTimeout = parsePingTimeout;
    function parseAudioSinkSelection(value) {
        if (value === "")
            return null;
        if (new TextEncoder().encode(value).length > 4096 || /[\u0000-\u001f\u007f-\u009f]/.test(value))
            throw new Error("The audio endpoint ID must contain at most 4096 UTF-8 bytes and no control characters.");
        return value;
    }
    Amaterasu.parseAudioSinkSelection = parseAudioSinkSelection;
    function parseExternalIp(value) {
        if (value === "")
            return null;
        if (new TextEncoder().encode(value).length > 4096 || /[\u0000-\u001f\u007f-\u009f]/.test(value))
            throw new Error("External IP text must contain at most 4096 UTF-8 bytes and no control characters.");
        return value;
    }
    Amaterasu.parseExternalIp = parseExternalIp;
    function parseWgcPacingSmoothing(value) {
        if (value === "")
            return null;
        if (value === "true")
            return true;
        if (value === "false")
            return false;
        throw new Error("Choose the default, enabled, or disabled WGC pacing policy.");
    }
    Amaterasu.parseWgcPacingSmoothing = parseWgcPacingSmoothing;
    function parseSteamExclusions(value) {
        let parsed;
        try {
            parsed = JSON.parse(value);
        }
        catch {
            throw new Error("Excluded games must be valid JSON.");
        }
        if (!Array.isArray(parsed) || parsed.length > 256)
            throw new Error("Excluded games must be a JSON array with at most 256 entries.");
        const ids = new Set();
        const names = new Set();
        for (const item of parsed) {
            if (typeof item !== "object" || item === null || Array.isArray(item))
                throw new Error("Each excluded game must be an object.");
            const exclusion = item;
            if (Object.keys(exclusion).some((key) => key !== "id" && key !== "name"))
                throw new Error("Excluded games may only contain id and name fields.");
            const id = exclusion.id;
            const name = exclusion.name;
            if (id === undefined && name === undefined)
                throw new Error("Each excluded game needs an app ID or name.");
            if (id !== undefined) {
                if (typeof id !== "string" ||
                    !/^[1-9][0-9]*$/.test(id) ||
                    !Number.isSafeInteger(Number(id)) ||
                    Number(id) > 0xffff_ffff ||
                    String(Number(id)) !== id)
                    throw new Error("Steam app IDs must be canonical nonzero decimal strings.");
                if (ids.has(id))
                    throw new Error("Excluded Steam app IDs must be unique.");
                ids.add(id);
            }
            if (name !== undefined) {
                if (typeof name !== "string" ||
                    name.trim().length === 0 ||
                    new TextEncoder().encode(name).length > 512 ||
                    /[\u0000-\u001f\u007f-\u009f]/.test(name))
                    throw new Error("Exclusion names must contain 1–512 printable UTF-8 bytes.");
                if (id === undefined) {
                    const normalized = name.toLowerCase();
                    if (names.has(normalized))
                        throw new Error("Name-only exclusions must be unique, ignoring case.");
                    names.add(normalized);
                }
            }
        }
        return parsed;
    }
    Amaterasu.parseSteamExclusions = parseSteamExclusions;
    function parseLutrisExclusions(value) {
        let parsed;
        try {
            parsed = JSON.parse(value);
        }
        catch {
            throw new Error("Lutris exclusions must be valid JSON.");
        }
        if (!Array.isArray(parsed) || parsed.length > 256)
            throw new Error("Lutris exclusions must be a JSON array with at most 256 entries.");
        const identities = new Set();
        const validateId = (id) => {
            if (id.length === 0 ||
                new TextEncoder().encode(id).length > 512 ||
                !/^[0-9]+$/.test(id) ||
                BigInt(id) === 0n ||
                BigInt(id) > 18446744073709551615n)
                throw new Error("Lutris exclusion IDs must be positive decimal integers.");
        };
        const validateName = (name) => {
            if (name.trim().length === 0 ||
                new TextEncoder().encode(name).length > 512 ||
                /[\u0000-\u001f\u007f-\u009f]/.test(name))
                throw new Error("Lutris exclusion names must be printable text of at most 512 UTF-8 bytes.");
        };
        const unique = (identity) => {
            if (identities.has(identity))
                throw new Error("Lutris exclusion IDs and name-only entries must be unique.");
            identities.add(identity);
        };
        for (const item of parsed) {
            if (typeof item === "string") {
                validateId(item);
                unique(`id:${item}`);
                continue;
            }
            if (typeof item !== "object" || item === null || Array.isArray(item))
                throw new Error("Each Lutris exclusion must be an ID string or object.");
            const exclusion = item;
            if (Object.keys(exclusion).some((key) => key !== "id" && key !== "name"))
                throw new Error("Lutris exclusions may only contain id and name fields.");
            const id = exclusion.id;
            const name = exclusion.name;
            if (id === undefined && name === undefined)
                throw new Error("Each Lutris exclusion needs an ID or name.");
            if (id !== undefined && typeof id !== "string")
                throw new Error("Lutris exclusion IDs must be strings.");
            if (name !== undefined && typeof name !== "string")
                throw new Error("Lutris exclusion names must be strings.");
            if (typeof id === "string" && id !== "") {
                validateId(id);
                if (typeof name === "string" && name !== "")
                    validateName(name);
                unique(`id:${id}`);
            }
            else if (typeof name === "string") {
                validateName(name);
                unique(`name:${name.toLowerCase()}`);
            }
            else {
                throw new Error("Each Lutris exclusion needs a positive ID or nonempty name.");
            }
        }
        const maximumSettings = {
            enabled: true,
            auto_sync: true,
            autosync_remove_uninstalled: true,
            include_steam: true,
            exclude_games: parsed,
        };
        const maximumUpdate = {
            ...maximumSettings,
            expected_revision: Number.MAX_SAFE_INTEGER,
        };
        const encoder = new TextEncoder();
        if (encoder.encode(JSON.stringify(maximumSettings)).length > 12_000 ||
            encoder.encode(JSON.stringify(maximumUpdate)).length > 12_000)
            throw new Error("Lutris exclusions exceed the product settings or management request size limit.");
        return parsed;
    }
    Amaterasu.parseLutrisExclusions = parseLutrisExclusions;
    function parseSteamSettingsReply(value) {
        const reply = parseImportSettingsEnvelope(value, "Steam");
        const values = reply.values;
        const imported = parseSteamImportSettings(values.include_tools, values.sync_all_installed, values.autosync_remove_uninstalled, values.recent_games, values.recent_max_age_days);
        if (typeof values.enabled !== "boolean" || typeof values.auto_sync !== "boolean")
            throw new Error("Steam settings reply contains invalid switches.");
        const parsed = {
            enabled: values.enabled,
            auto_sync: values.auto_sync,
            exclude_games: parseSteamExclusions(JSON.stringify(values.exclude_games)),
            ...imported,
        };
        if (!sameConfigurationValue(values, parsed))
            throw new Error("Steam settings reply contains unexpected values.");
        return { ok: true, revision: reply.revision, values: parsed };
    }
    Amaterasu.parseSteamSettingsReply = parseSteamSettingsReply;
    function parseLutrisSettingsReply(value) {
        const reply = parseImportSettingsEnvelope(value, "Lutris");
        const values = reply.values;
        if (typeof values.enabled !== "boolean" || typeof values.auto_sync !== "boolean" ||
            typeof values.autosync_remove_uninstalled !== "boolean" ||
            typeof values.include_steam !== "boolean")
            throw new Error("Lutris settings reply contains invalid switches.");
        const parsed = {
            enabled: values.enabled,
            auto_sync: values.auto_sync,
            autosync_remove_uninstalled: values.autosync_remove_uninstalled,
            include_steam: values.include_steam,
            exclude_games: parseLutrisExclusions(JSON.stringify(values.exclude_games)),
        };
        if (!sameConfigurationValue(values, parsed))
            throw new Error("Lutris settings reply contains unexpected values.");
        return { ok: true, revision: reply.revision, values: parsed };
    }
    Amaterasu.parseLutrisSettingsReply = parseLutrisSettingsReply;
    function parseImportSettingsEnvelope(value, provider) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error(`${provider} settings reply is invalid.`);
        const reply = value;
        if (reply.ok !== true || !Number.isSafeInteger(reply.revision) ||
            reply.revision < 0 || !reply.values ||
            typeof reply.values !== "object" || Array.isArray(reply.values))
            throw new Error(`${provider} settings reply is invalid.`);
        return { revision: reply.revision, values: reply.values };
    }
    async function readImportSettings(api, provider, parse) {
        for (let attempt = 0;; attempt += 1) {
            try {
                return parse(await api.post(`${provider}/settings`));
            }
            catch (error) {
                if (!(error instanceof Amaterasu.ApiError) ||
                    !["settings_loading", "settings_busy"].includes(error.code) || attempt >= 7)
                    throw error;
                await new Promise((resolve) => window.setTimeout(resolve, 50 * (attempt + 1)));
            }
        }
    }
    async function saveImportSettings(api, provider, current, values, parse) {
        if (!Number.isSafeInteger(current.revision) || current.revision < 0 ||
            current.revision >= Number.MAX_SAFE_INTEGER)
            throw new Error(`${provider} settings revision is invalid.`);
        const confirms = (reply) => {
            if (reply.revision !== current.revision + 1)
                return false;
            if (sameConfigurationValue(reply.values, values))
                return true;
            return provider === "steam" && values.enabled === false && reply.values.enabled === true &&
                sameConfigurationValue({ ...reply.values, enabled: false }, values);
        };
        let updated;
        try {
            updated = parse(await api.post(`${provider}/settings_update`, {
                ...values,
                expected_revision: current.revision,
            }));
        }
        catch (error) {
            if (!(error instanceof Amaterasu.ApiError) ||
                (provider === "steam" ? error.code !== "settings_outcome_unknown" : error.code !== "update_outcome_unknown"))
                throw error;
            const reconciled = await readImportSettings(api, provider, parse);
            if (!confirms(reconciled))
                throw new Error(`${provider} update outcome is uncertain. Refresh and check the current values.`);
            return reconciled;
        }
        if (!confirms(updated))
            throw new Error(`${provider} update could not be confirmed. Refresh and check the current values.`);
        const saved = await readImportSettings(api, provider, parse);
        if (!confirms(saved) || !sameConfigurationValue(saved.values, updated.values))
            throw new Error(`${provider} settings could not be confirmed after saving. Refresh and check the current values.`);
        return saved;
    }
    function readSteamSettings(api) {
        return readImportSettings(api, "steam", parseSteamSettingsReply);
    }
    Amaterasu.readSteamSettings = readSteamSettings;
    function readLutrisSettings(api) {
        return readImportSettings(api, "lutris", parseLutrisSettingsReply);
    }
    Amaterasu.readLutrisSettings = readLutrisSettings;
    function saveSteamSettings(api, current, values) {
        return saveImportSettings(api, "steam", current, values, parseSteamSettingsReply);
    }
    Amaterasu.saveSteamSettings = saveSteamSettings;
    function saveLutrisSettings(api, current, values) {
        return saveImportSettings(api, "lutris", current, values, parseLutrisSettingsReply);
    }
    Amaterasu.saveLutrisSettings = saveLutrisSettings;
    class ImportSettingsLoadGate {
        generation = 0;
        invalidate() {
            this.generation += 1;
        }
        isCurrent(generation) {
            return generation === this.generation;
        }
        async read(load) {
            const generation = ++this.generation;
            try {
                const value = await load();
                return this.isCurrent(generation) ? { value, generation } : null;
            }
            catch (error) {
                if (!this.isCurrent(generation))
                    return null;
                throw error;
            }
        }
    }
    Amaterasu.ImportSettingsLoadGate = ImportSettingsLoadGate;
    function lutrisStatusDescription(status) {
        if (!status.settings_ready)
            return "Lutris settings are still loading; discovery state is not confirmed.";
        if (!status.platform_supported)
            return "Lutris discovery is supported only on Linux and is inactive on this platform.";
        if (!status.enabled)
            return "Lutris discovery is disabled.";
        if (!status.database_available)
            return "The current user's Lutris game database is unavailable.";
        if (!status.executable_available)
            return "The Lutris executable was not found in the current user's PATH.";
        if (status.last_error)
            return `Lutris discovery is available · ${status.game_count} games · Last scan issue: ${status.last_error}`;
        const pending = status.refresh_pending ? "A catalog scan is in progress." : "No catalog scan is pending.";
        return `Lutris discovery is available · ${status.game_count} games · ${pending}`;
    }
    Amaterasu.lutrisStatusDescription = lutrisStatusDescription;
    function parsePlayniteFilterList(source, label) {
        let parsed;
        try {
            parsed = JSON.parse(source);
        }
        catch {
            throw new Error(`${label} must be a JSON array of strings.`);
        }
        return validatePlayniteFilterList(parsed, label);
    }
    Amaterasu.parsePlayniteFilterList = parsePlayniteFilterList;
    function parsePlayniteSettings(values) {
        if (typeof values.enabled !== "boolean" ||
            typeof values.auto_sync !== "boolean" ||
            typeof values.sync_all_installed !== "boolean" ||
            typeof values.autosync_remove_uninstalled !== "boolean" ||
            typeof values.autosync_require_replacement !== "boolean" ||
            typeof values.focus_exit_on_first !== "boolean" ||
            typeof values.fullscreen_entry_enabled !== "boolean" ||
            !Number.isSafeInteger(values.autosync_delete_after_days) ||
            values.autosync_delete_after_days < 0 ||
            values.autosync_delete_after_days > 2_147_483_647 ||
            !Number.isSafeInteger(values.focus_attempts) ||
            values.focus_attempts < 0 ||
            values.focus_attempts > 2_147_483_647 ||
            !Number.isSafeInteger(values.focus_timeout_secs) ||
            values.focus_timeout_secs < 0 ||
            values.focus_timeout_secs > 2_147_483_647 ||
            !Number.isSafeInteger(values.recent_games) ||
            values.recent_games < 0 ||
            values.recent_games > 10_000 ||
            !Number.isSafeInteger(values.recent_max_age_days) ||
            values.recent_max_age_days < 0 ||
            values.recent_max_age_days > 36_500)
            throw new Error("Playnite settings contain values outside their supported ranges.");
        const parsed = {
            enabled: values.enabled,
            auto_sync: values.auto_sync,
            sync_all_installed: values.sync_all_installed,
            autosync_remove_uninstalled: values.autosync_remove_uninstalled,
            autosync_delete_after_days: values.autosync_delete_after_days,
            autosync_require_replacement: values.autosync_require_replacement,
            focus_attempts: values.focus_attempts,
            focus_timeout_secs: values.focus_timeout_secs,
            focus_exit_on_first: values.focus_exit_on_first,
            fullscreen_entry_enabled: values.fullscreen_entry_enabled,
            recent_games: values.recent_games,
            recent_max_age_days: values.recent_max_age_days,
            sync_categories: validatePlayniteFilterList(values.sync_categories, "Sync categories"),
            exclude_categories: validatePlayniteFilterList(values.exclude_categories, "Excluded categories"),
            sync_plugins: validatePlayniteFilterList(values.sync_plugins, "Sync plugin IDs"),
            exclude_plugins: validatePlayniteFilterList(values.exclude_plugins, "Excluded plugin IDs"),
            exclude_games: validatePlayniteFilterList(values.exclude_games, "Excluded game IDs"),
        };
        const maximumUpdate = { ...parsed, expected_revision: Number.MAX_SAFE_INTEGER };
        const encoder = new TextEncoder();
        if (encoder.encode(JSON.stringify(parsed)).length > 12_000 ||
            encoder.encode(JSON.stringify(maximumUpdate)).length > 12_000)
            throw new Error("Playnite settings exceed the 12000 byte document or request size limit.");
        return parsed;
    }
    Amaterasu.parsePlayniteSettings = parsePlayniteSettings;
    function parsePlayniteSettingsReply(value) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Playnite settings reply is invalid.");
        const reply = value;
        if (reply.ok !== true || !Number.isSafeInteger(reply.revision) ||
            reply.revision < 0 || !reply.values ||
            typeof reply.values !== "object" || Array.isArray(reply.values))
            throw new Error("Playnite settings reply is invalid.");
        const values = reply.values;
        const parsed = parsePlayniteSettings(values);
        if (!sameConfigurationValue(values, parsed))
            throw new Error("Playnite settings reply contains unexpected values.");
        return { ok: true, revision: reply.revision, values: parsed };
    }
    Amaterasu.parsePlayniteSettingsReply = parsePlayniteSettingsReply;
    async function readPlayniteSettings(api) {
        for (let attempt = 0;; attempt += 1) {
            try {
                return parsePlayniteSettingsReply(await api.post("playnite/settings"));
            }
            catch (error) {
                if (!(error instanceof Amaterasu.ApiError) ||
                    !["settings_loading", "settings_busy"].includes(error.code) || attempt >= 7)
                    throw error;
                await new Promise((resolve) => window.setTimeout(resolve, 50 * (attempt + 1)));
            }
        }
    }
    Amaterasu.readPlayniteSettings = readPlayniteSettings;
    async function savePlayniteSettings(api, current, values) {
        if (!Number.isSafeInteger(current.revision) || current.revision < 0 ||
            current.revision >= Number.MAX_SAFE_INTEGER)
            throw new Error("Playnite settings revision is invalid.");
        const requested = parsePlayniteSettings(values);
        const confirms = (reply) => reply.revision === current.revision + 1 &&
            sameConfigurationValue(reply.values, requested);
        let updated;
        try {
            updated = parsePlayniteSettingsReply(await api.post("playnite/settings_update", {
                ...requested,
                expected_revision: current.revision,
            }));
        }
        catch (error) {
            if (!(error instanceof Amaterasu.ApiError) || error.code !== "update_outcome_unknown")
                throw error;
            const reconciled = await readPlayniteSettings(api);
            if (!confirms(reconciled))
                throw new Error("Playnite update outcome is uncertain. Refresh and check the current values.");
            return reconciled;
        }
        if (!confirms(updated))
            throw new Error("Playnite update could not be confirmed. Refresh and check the current values.");
        const saved = await readPlayniteSettings(api);
        if (!confirms(saved))
            throw new Error("Playnite settings could not be confirmed after saving. Refresh and check the current values.");
        return saved;
    }
    Amaterasu.savePlayniteSettings = savePlayniteSettings;
    class PlayniteSettingsLoadGate {
        generation = 0;
        invalidate() {
            this.generation += 1;
        }
        async read(loadSettings, loadStatus) {
            const generation = ++this.generation;
            try {
                const settings = await loadSettings();
                if (generation !== this.generation)
                    return null;
                const runtime = await loadStatus();
                return generation === this.generation ? { settings, runtime } : null;
            }
            catch (error) {
                if (generation !== this.generation)
                    return null;
                throw error;
            }
        }
    }
    Amaterasu.PlayniteSettingsLoadGate = PlayniteSettingsLoadGate;
    function playniteStatusDescription(status) {
        if (!status.settings_ready)
            return "Playnite settings are still loading; runtime state is not confirmed.";
        if (!status.enabled)
            return "Playnite catalog synchronization is disabled.";
        if (!status.playnite_running)
            return "No Playnite process was detected. Process discovery is currently available on Windows.";
        if (status.last_error)
            return `Playnite is running · ${status.snapshot_game_count} games in the last snapshot · Last issue: ${status.last_error}`;
        const sync = status.snapshot_pending
            ? "A catalog snapshot is in progress."
            : "No catalog snapshot is in progress.";
        return `Playnite is running · ${status.snapshot_game_count} games in the last snapshot · ${sync}`;
    }
    Amaterasu.playniteStatusDescription = playniteStatusDescription;
    function validatePlayniteFilterList(value, label) {
        if (!Array.isArray(value) || value.length > 256)
            throw new Error(`${label} must contain at most 256 strings.`);
        const encoder = new TextEncoder();
        for (const item of value) {
            if (typeof item !== "string" ||
                item.trim().length === 0 ||
                encoder.encode(item).length > 512 ||
                /[\u0000-\u001f\u007f-\u009f]/.test(item))
                throw new Error(`${label} must contain printable strings of at most 512 UTF-8 bytes.`);
        }
        return [...value];
    }
    function parseSteamImportSettings(includeTools, syncAllInstalled, autosyncRemoveUninstalled, recentGames, recentMaxAgeDays) {
        if (typeof includeTools !== "boolean" ||
            typeof syncAllInstalled !== "boolean" ||
            typeof autosyncRemoveUninstalled !== "boolean" ||
            !Number.isSafeInteger(recentGames) ||
            recentGames < 0 ||
            recentGames > 10_000 ||
            !Number.isSafeInteger(recentMaxAgeDays) ||
            recentMaxAgeDays < 0 ||
            recentMaxAgeDays > 36_500)
            throw new Error("Steam import values are outside their supported ranges.");
        return {
            include_tools: includeTools,
            sync_all_installed: syncAllInstalled,
            autosync_remove_uninstalled: autosyncRemoveUninstalled,
            recent_games: recentGames,
            recent_max_age_days: recentMaxAgeDays,
        };
    }
    Amaterasu.parseSteamImportSettings = parseSteamImportSettings;
    function steamStatusDescription(status) {
        if (status.settings_ready === false)
            return "Provider settings are still loading; runtime state is not confirmed yet.";
        if (typeof status.enabled !== "boolean")
            return "The provider did not report whether Steam discovery is enabled.";
        if (!status.enabled)
            return "Steam discovery is disabled; no scan is running.";
        const scanState = status.scanning
            ? "scan in progress"
            : status.complete
                ? "last scan complete"
                : "no complete scan is available";
        const warningCount = `${status.warning_count} ${status.warning_count === 1 ? "warning" : "warnings"}`;
        return `Steam discovery enabled · ${scanState} · ${status.game_count} games · ${warningCount}.`;
    }
    Amaterasu.steamStatusDescription = steamStatusDescription;
    function renderSettings(context) {
        const area = Amaterasu.el("div");
        const registeredLoads = new RegisteredConfigurationLoadGate();
        const authArea = Amaterasu.el("div");
        const authLoads = new AuthTokenSettingsLoadGate();
        const hostLoggingArea = Amaterasu.el("div");
        const hostLoggingLoads = new HostLoggingLoadGate();
        const hostArea = Amaterasu.el("div");
        const hostLoads = new NvhttpSettingsLoadGate();
        const steamArea = Amaterasu.el("div");
        const steamLoads = new ImportSettingsLoadGate();
        const lutrisArea = Amaterasu.el("div");
        const lutrisLoads = new ImportSettingsLoadGate();
        const playniteArea = Amaterasu.el("div");
        const playniteLoads = new PlayniteSettingsLoadGate();
        const inputArea = Amaterasu.el("div");
        const inputLoads = new InputPolicyLoadGate();
        const historyArea = Amaterasu.el("div");
        const historyLoads = new HistorySettingsLoadGate();
        const pairingArea = Amaterasu.el("div");
        const pairingLoads = new PairingClientsLoadGate();
        context.content.append(Amaterasu.sectionHeading("Settings", "Edit host options and values registered for this management console.", Amaterasu.button("Refresh", () => {
            void load();
            void loadAuth();
            void loadHostLogging();
            void loadHost();
            void loadSteam();
            void loadLutris();
            void loadPlaynite();
            void loadInput();
            void loadHistorySettings();
            void loadPairing();
        })), area, authArea, hostLoggingArea, hostArea, steamArea, lutrisArea, playniteArea, inputArea, historyArea, pairingArea);
        async function loadAuth() {
            authArea.replaceChildren(Amaterasu.el("p", "muted", "Loading authentication lifetimes…"));
            try {
                const settings = await authLoads.read(context.api);
                if (settings === null)
                    return;
                if (!context.content.isConnected)
                    return;
                const form = Amaterasu.el("form", "settings-form");
                const sessionTtl = Amaterasu.input("session_token_ttl_seconds", String(settings.values.session_token_ttl_seconds), "number");
                const refreshTtl = Amaterasu.input("remember_me_refresh_token_ttl_seconds", String(settings.values.remember_me_refresh_token_ttl_seconds), "number");
                for (const control of [sessionTtl, refreshTtl]) {
                    control.required = true;
                    control.max = String(maxConfiguredAuthTtlSeconds);
                }
                sessionTtl.min = "1";
                sessionTtl.step = "1";
                refreshTtl.min = "1";
                refreshTtl.step = "1";
                const status = Amaterasu.el("div");
                const save = Amaterasu.el("button", "button primary", "Save authentication settings");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                form.append(Amaterasu.field("Administrator session lifetime (seconds)", sessionTtl, "Access sessions default to 7200 seconds. Existing sessions keep their issued expiry; new logins and refreshes use this value."), Amaterasu.field("Remembered device lifetime (seconds)", refreshTtl, "Remember-me refresh tokens default to 604800 seconds. Non-remembered refresh tokens remain fixed at 86400 seconds."), status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${settings.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, status, async () => {
                        authLoads.invalidate();
                        try {
                            await saveAuthTokenLifetimes(context.api, settings.revision, sessionTtl.valueAsNumber, refreshTtl.valueAsNumber);
                        }
                        catch (error) {
                            if (error instanceof Amaterasu.ApiError && error.code === "revision_conflict") {
                                await loadAuth();
                                context.notify("Authentication settings changed elsewhere. Review the latest values and save again.");
                                return;
                            }
                            throw error;
                        }
                        await loadAuth();
                        context.notify("Authentication lifetimes saved and confirmed.");
                    });
                });
                authArea.replaceChildren(Amaterasu.panel("Authentication session lifetimes", form));
            }
            catch (error) {
                if (context.content.isConnected)
                    authArea.replaceChildren(Amaterasu.errorPanel(error, loadAuth));
            }
        }
        async function loadHostLogging() {
            hostLoggingArea.replaceChildren(Amaterasu.el("p", "muted", "Loading host logging policy…"));
            try {
                const state = await hostLoggingLoads.read(context.api);
                if (state === null)
                    return;
                const { settings, diagnostics: diagnosticsStatus } = state;
                if (!context.content.isConnected)
                    return;
                const form = Amaterasu.el("form", "settings-form");
                const level = Amaterasu.el("select");
                level.name = "min_log_level";
                for (const optionValue of hostLogLevels) {
                    const option = Amaterasu.el("option", "", optionValue.replaceAll("_", " "));
                    option.value = optionValue;
                    level.append(option);
                }
                level.value = settings.values.min_log_level;
                const path = Amaterasu.input("log_path", settings.values.log_path);
                path.required = true;
                path.maxLength = 4096;
                const status = Amaterasu.el("div");
                const applicationStatus = Amaterasu.el("p", "muted", diagnosticsLoggingDescription(settings.revision, diagnosticsStatus));
                const save = Amaterasu.el("button", "button primary", "Save host logging settings");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                const refresh = Amaterasu.button("Refresh diagnostics status", () => {
                    void Amaterasu.runAction(refresh, status, async () => {
                        const current = await readDiagnosticsLoggingStatus(context.api);
                        applicationStatus.textContent = diagnosticsLoggingDescription(settings.revision, current);
                    });
                });
                refresh.disabled = !context.api.allows("diagnostics.read");
                form.append(Amaterasu.field("Minimum log level", level, "Product default uses the release channel's policy. Diagnostic records below this level are filtered."), Amaterasu.field("Log path", path, "A relative path stays beneath the private host state root. The setting applies to diagnostic records when diagnostics confirms the policy."), Amaterasu.el("p", "muted", `Effective host policy: ${settings.effective.min_log_level} · ${settings.effective.log_path}`), Amaterasu.el("p", "muted", "The host confirms logging application during save; diagnostics status checks disk delivery. Other host and native logs do not use this sink; a host restart may be required for their separate logging policies."), applicationStatus, refresh, status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${settings.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    if (!form.reportValidity())
                        return;
                    void Amaterasu.runAction(save, status, async () => {
                        hostLoggingLoads.invalidate();
                        try {
                            const saved = await saveHostLoggingState(context.api, settings, level.value, path.value);
                            context.notify(diagnosticsLoggingDescription(saved.settings.revision, saved.diagnostics));
                        }
                        catch (error) {
                            if (error instanceof Amaterasu.ApiError && error.code === "revision_conflict") {
                                await loadHostLogging();
                                context.notify("Host configuration changed elsewhere. Review the latest logging settings and save again.");
                                return;
                            }
                            await loadHostLogging();
                            context.notify(hostLoggingSaveFailureNotice(error instanceof Amaterasu.ApiError ? error.code : null));
                            return;
                        }
                        await loadHostLogging();
                    });
                });
                hostLoggingArea.replaceChildren(Amaterasu.panel("Host logging", form));
            }
            catch {
                if (context.content.isConnected)
                    hostLoggingArea.replaceChildren(Amaterasu.errorPanel(new Error("Host logging policy could not be loaded. Try again."), loadHostLogging));
            }
        }
        async function loadSteam() {
            steamArea.replaceChildren(Amaterasu.el("p", "muted", "Loading Steam settings…"));
            try {
                const loaded = await steamLoads.read(() => readSteamSettings(context.api));
                if (loaded === null)
                    return;
                const { value: settings, generation } = loaded;
                if (!context.content.isConnected)
                    return;
                const form = Amaterasu.el("form", "settings-form");
                const enabled = Amaterasu.input("steam_enabled", "", "checkbox");
                enabled.checked = settings.values.enabled;
                const autoSync = Amaterasu.input("steam_auto_sync", "", "checkbox");
                autoSync.checked = settings.values.auto_sync;
                const includeTools = Amaterasu.input("steam_include_tools", "", "checkbox");
                includeTools.checked = settings.values.include_tools;
                const syncAllInstalled = Amaterasu.input("steam_sync_all_installed", "", "checkbox");
                syncAllInstalled.checked = settings.values.sync_all_installed;
                const autosyncRemoveUninstalled = Amaterasu.input("steam_autosync_remove_uninstalled", "", "checkbox");
                autosyncRemoveUninstalled.checked =
                    settings.values.autosync_remove_uninstalled;
                const recentGames = Amaterasu.input("steam_recent_games", String(settings.values.recent_games), "number");
                recentGames.required = true;
                recentGames.min = "0";
                recentGames.max = "10000";
                recentGames.step = "1";
                const recentMaxAgeDays = Amaterasu.input("steam_recent_max_age_days", String(settings.values.recent_max_age_days), "number");
                recentMaxAgeDays.required = true;
                recentMaxAgeDays.min = "0";
                recentMaxAgeDays.max = "36500";
                recentMaxAgeDays.step = "1";
                const exclusions = Amaterasu.el("textarea");
                exclusions.name = "steam_exclude_games";
                exclusions.rows = 10;
                exclusions.spellcheck = false;
                exclusions.value = JSON.stringify(settings.values.exclude_games, null, 2);
                const status = Amaterasu.el("div");
                const runtimeStatus = Amaterasu.el("p", "muted", "Loading current provider status…");
                const save = Amaterasu.el("button", "button primary", "Save Steam settings");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                form.append(Amaterasu.field("Enable Steam library discovery", enabled, "Controls whether the provider exposes discovered Steam games and accepts scans. On Linux the provider keeps discovery enabled."), Amaterasu.field("Automatic synchronization", autoSync, "Controls automatic scans of the configured Steam library roots."), Amaterasu.field("Include tools", includeTools, "Includes tools alongside games when importing the library."), Amaterasu.field("Synchronize all installed games", syncAllInstalled, "When off, import recently played games instead of every installed game."), Amaterasu.field("Remove uninstalled games during synchronization", autosyncRemoveUninstalled, "Applies when synchronizing all installed games. Recent-game mode always removes stale imported entries."), Amaterasu.field("Recent games to import", recentGames, "Maximum number of recently played games (0–10000); zero imports none in recent-only mode."), Amaterasu.field("Recent game age limit (days)", recentMaxAgeDays, "Ignore games older than this age (0–36500); zero disables the age cutoff."), Amaterasu.field("Excluded games (JSON)", exclusions, 'Use objects with "id" (canonical app ID), "name", or both. ID takes precedence; name-only entries match without case sensitivity.'), runtimeStatus, status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${settings.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, status, async () => {
                        steamLoads.invalidate();
                        let saveStarted = false;
                        try {
                            const values = {
                                enabled: enabled.checked,
                                auto_sync: autoSync.checked,
                                exclude_games: parseSteamExclusions(exclusions.value),
                                ...parseSteamImportSettings(includeTools.checked, syncAllInstalled.checked, autosyncRemoveUninstalled.checked, recentGames.valueAsNumber, recentMaxAgeDays.valueAsNumber),
                            };
                            saveStarted = true;
                            const saved = await saveSteamSettings(context.api, settings, values);
                            await loadSteam();
                            context.notify(!values.enabled && saved.values.enabled
                                ? "Steam settings saved and confirmed. Steam discovery remains enabled by Linux host policy; scan status is shown below."
                                : "Steam settings saved and confirmed. Scan status is shown below.");
                        }
                        catch (error) {
                            if (saveStarted) {
                                await loadSteam();
                                context.notify(error instanceof Amaterasu.ApiError && error.code === "revision_conflict"
                                    ? "Steam settings changed elsewhere. Review the latest values before saving again."
                                    : "Steam save could not be confirmed. Review the latest values before saving again.");
                                return;
                            }
                            throw error;
                        }
                    });
                });
                steamArea.replaceChildren(Amaterasu.panel("Steam", form));
                void loadSteamRuntimeStatus(runtimeStatus, context, settings, generation);
            }
            catch (error) {
                if (context.content.isConnected)
                    steamArea.replaceChildren(Amaterasu.errorPanel(error, loadSteam));
            }
        }
        async function loadLutris() {
            lutrisArea.replaceChildren(Amaterasu.el("p", "muted", "Loading Lutris settings…"));
            try {
                const loaded = await lutrisLoads.read(async () => {
                    const settings = await readLutrisSettings(context.api);
                    const runtime = await context.api.post("lutris/status");
                    return { settings, runtime };
                });
                if (loaded === null)
                    return;
                const { settings, runtime } = loaded.value;
                if (!context.content.isConnected)
                    return;
                const form = Amaterasu.el("form", "settings-form");
                const enabled = Amaterasu.input("lutris_enabled", "", "checkbox");
                enabled.checked = settings.values.enabled;
                enabled.disabled = !runtime.platform_supported;
                const autoSync = Amaterasu.input("lutris_auto_sync", "", "checkbox");
                autoSync.checked = settings.values.auto_sync;
                const removeUninstalled = Amaterasu.input("lutris_autosync_remove_uninstalled", "", "checkbox");
                removeUninstalled.checked = settings.values.autosync_remove_uninstalled;
                const includeSteam = Amaterasu.input("lutris_include_steam", "", "checkbox");
                includeSteam.checked = settings.values.include_steam;
                const exclusions = Amaterasu.el("textarea");
                exclusions.name = "lutris_exclude_games";
                exclusions.rows = 8;
                exclusions.maxLength = 12_000;
                exclusions.spellcheck = false;
                exclusions.value = JSON.stringify(settings.values.exclude_games, null, 2);
                const runtimeStatus = Amaterasu.el("p", runtime.last_error ? "error" : "muted", runtime.settings_revision !== settings.revision ||
                    runtime.enabled !== settings.values.enabled ||
                    runtime.auto_sync !== settings.values.auto_sync
                    ? "Saved Lutris settings are not yet reflected in provider status. Refresh to check application."
                    : lutrisStatusDescription(runtime));
                const saveStatus = Amaterasu.el("div");
                const refreshStatus = Amaterasu.el("div");
                const save = Amaterasu.el("button", "button primary", "Save Lutris settings");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                const refresh = Amaterasu.button("Refresh now", () => Amaterasu.runAction(refresh, refreshStatus, async () => {
                    const updated = await context.api.post("lutris/refresh");
                    if (!runtimeStatus.isConnected || !lutrisLoads.isCurrent(loaded.generation))
                        return;
                    runtimeStatus.className = updated.last_error ? "error" : "muted";
                    runtimeStatus.textContent = lutrisStatusDescription(updated);
                    context.notify("Lutris refresh requested.");
                }));
                refresh.disabled =
                    !context.api.allows("configuration.write") ||
                        !runtime.platform_supported ||
                        !settings.values.enabled;
                enabled.addEventListener("change", () => {
                    refresh.disabled =
                        !context.api.allows("configuration.write") ||
                            !runtime.platform_supported ||
                            !enabled.checked;
                });
                form.append(Amaterasu.field("Enable Lutris discovery", enabled, runtime.platform_supported
                    ? "Controls discovery for the current Linux user."
                    : "Lutris discovery is available only on Linux; this option cannot be enabled on this platform."), Amaterasu.field("Automatic synchronization", autoSync, "Scan the current user's installed Lutris games automatically."), Amaterasu.field("Remove uninstalled games", removeUninstalled, "After a successful complete scan, remove Lutris-owned games that are no longer installed."), Amaterasu.field("Include matching Steam games", includeSteam, "Read a complete Steam catalog before import; the direct Steam catalog record takes precedence for matching app IDs."), Amaterasu.field("Excluded games (JSON)", exclusions, 'Use a positive decimal ID string or an object with "id", "name", or both. ID takes precedence; name-only entries match without case sensitivity.'), runtimeStatus, saveStatus, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${settings.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, saveStatus, async () => {
                        lutrisLoads.invalidate();
                        let saveStarted = false;
                        try {
                            const values = {
                                enabled: enabled.checked,
                                auto_sync: autoSync.checked,
                                autosync_remove_uninstalled: removeUninstalled.checked,
                                include_steam: includeSteam.checked,
                                exclude_games: parseLutrisExclusions(exclusions.value),
                            };
                            saveStarted = true;
                            await saveLutrisSettings(context.api, settings, values);
                            await loadLutris();
                            context.notify("Lutris settings saved and confirmed. Scan status is shown below.");
                        }
                        catch (error) {
                            if (saveStarted) {
                                await loadLutris();
                                context.notify(error instanceof Amaterasu.ApiError && error.code === "revision_conflict"
                                    ? "Lutris settings changed elsewhere. Review the latest values before saving again."
                                    : "Lutris save could not be confirmed. Review the latest values before saving again.");
                                return;
                            }
                            throw error;
                        }
                    });
                });
                refreshStatus.className = "muted";
                lutrisArea.replaceChildren(Amaterasu.panel("Lutris", form, Amaterasu.el("div", "actions", refresh, refreshStatus)));
            }
            catch (error) {
                if (context.content.isConnected)
                    lutrisArea.replaceChildren(Amaterasu.errorPanel(error, loadLutris));
            }
        }
        async function loadPlaynite() {
            playniteArea.replaceChildren(Amaterasu.el("p", "muted", "Loading Playnite settings…"));
            try {
                const loaded = await playniteLoads.read(() => readPlayniteSettings(context.api), () => context.api.post("playnite/status"));
                if (loaded === null)
                    return;
                const { settings, runtime } = loaded;
                if (!context.content.isConnected)
                    return;
                const form = Amaterasu.el("form", "settings-form");
                const enabled = Amaterasu.input("playnite_enabled", "", "checkbox");
                enabled.checked = settings.values.enabled;
                const autoSync = Amaterasu.input("playnite_auto_sync", "", "checkbox");
                autoSync.checked = settings.values.auto_sync;
                const syncAllInstalled = Amaterasu.input("playnite_sync_all_installed", "", "checkbox");
                syncAllInstalled.checked = settings.values.sync_all_installed;
                const removeUninstalled = Amaterasu.input("playnite_autosync_remove_uninstalled", "", "checkbox");
                removeUninstalled.checked = settings.values.autosync_remove_uninstalled;
                const deleteAfterDays = Amaterasu.input("playnite_autosync_delete_after_days", String(settings.values.autosync_delete_after_days), "number");
                deleteAfterDays.required = true;
                deleteAfterDays.min = "0";
                deleteAfterDays.max = "2147483647";
                deleteAfterDays.step = "1";
                const requireReplacement = Amaterasu.input("playnite_autosync_require_replacement", "", "checkbox");
                requireReplacement.checked = settings.values.autosync_require_replacement;
                const focusAttempts = Amaterasu.input("playnite_focus_attempts", String(settings.values.focus_attempts), "number");
                focusAttempts.required = true;
                focusAttempts.min = "0";
                focusAttempts.max = "2147483647";
                focusAttempts.step = "1";
                const focusTimeoutSecs = Amaterasu.input("playnite_focus_timeout_secs", String(settings.values.focus_timeout_secs), "number");
                focusTimeoutSecs.required = true;
                focusTimeoutSecs.min = "0";
                focusTimeoutSecs.max = "2147483647";
                focusTimeoutSecs.step = "1";
                const focusExitOnFirst = Amaterasu.input("playnite_focus_exit_on_first", "", "checkbox");
                focusExitOnFirst.checked = settings.values.focus_exit_on_first;
                const fullscreenEntryEnabled = Amaterasu.input("playnite_fullscreen_entry_enabled", "", "checkbox");
                fullscreenEntryEnabled.checked = settings.values.fullscreen_entry_enabled;
                const recentGames = Amaterasu.input("playnite_recent_games", String(settings.values.recent_games), "number");
                recentGames.required = true;
                recentGames.min = "0";
                recentGames.max = "10000";
                recentGames.step = "1";
                const recentMaxAgeDays = Amaterasu.input("playnite_recent_max_age_days", String(settings.values.recent_max_age_days), "number");
                recentMaxAgeDays.required = true;
                recentMaxAgeDays.min = "0";
                recentMaxAgeDays.max = "36500";
                recentMaxAgeDays.step = "1";
                const filterList = (name, values) => {
                    const control = Amaterasu.el("textarea");
                    control.name = name;
                    control.rows = 5;
                    control.maxLength = 12_000;
                    control.spellcheck = false;
                    control.value = JSON.stringify(values, null, 2);
                    return control;
                };
                const syncCategories = filterList("playnite_sync_categories", settings.values.sync_categories);
                const excludeCategories = filterList("playnite_exclude_categories", settings.values.exclude_categories);
                const syncPlugins = filterList("playnite_sync_plugins", settings.values.sync_plugins);
                const excludePlugins = filterList("playnite_exclude_plugins", settings.values.exclude_plugins);
                const excludeGames = filterList("playnite_exclude_games", settings.values.exclude_games);
                const runtimeStatus = Amaterasu.el("p", runtime.last_error ? "error" : "muted", runtime.revision !== settings.revision || runtime.enabled !== settings.values.enabled ||
                    runtime.auto_sync !== settings.values.auto_sync
                    ? "Saved Playnite settings are not yet reflected in runtime status. Refresh to check application."
                    : playniteStatusDescription(runtime));
                const saveStatus = Amaterasu.el("div");
                const refreshStatus = Amaterasu.el("div");
                const save = Amaterasu.el("button", "button primary", "Save Playnite settings");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                const refresh = Amaterasu.button("Refresh now", () => Amaterasu.runAction(refresh, refreshStatus, async () => {
                    await context.api.post("playnite/refresh");
                    context.notify("Playnite refresh requested.");
                    await loadPlaynite();
                }));
                const updateRefreshAvailability = () => {
                    refresh.disabled =
                        !context.api.allows("configuration.write") ||
                            !enabled.checked ||
                            !runtime.playnite_running ||
                            runtime.snapshot_pending;
                };
                updateRefreshAvailability();
                enabled.addEventListener("change", updateRefreshAvailability);
                form.append(Amaterasu.field("Enable Playnite synchronization", enabled, "Controls whether Playnite games are imported. The setting is preserved on hosts where Playnite process discovery is unavailable."), Amaterasu.field("Automatic synchronization", autoSync, "Periodically scan Playnite while its desktop or fullscreen client is running."), Amaterasu.field("Synchronize all installed games", syncAllInstalled, "When off, import recent games plus installed games selected by category or plugin filters."), Amaterasu.field("Remove uninstalled Playnite games", removeUninstalled, "Remove imported games explicitly reported as uninstalled by a complete Playnite snapshot."), Amaterasu.field("Delete expired Playnite games after (days)", deleteAfterDays, "Remove eligible Playnite imports after this age when they have not been played since import. Zero disables this cleanup."), Amaterasu.field("Require a replacement for recent-game cleanup", requireReplacement, "Limit removal of unselected recent imports to the count of newly selected games."), Amaterasu.field("Focus attempts after game start", focusAttempts, "Try to focus the confirmed Playnite game process this many times. Zero disables focus."), Amaterasu.field("Focus timeout (seconds)", focusTimeoutSecs, "Stop trying after this many seconds. Zero disables focus."), Amaterasu.field("Stop focus attempts after first success", focusExitOnFirst, "When enabled, stop retrying once the game is confirmed in the foreground."), Amaterasu.field("Show Playnite Fullscreen in the catalog", fullscreenEntryEnabled, "Add or remove the provider-managed Playnite Fullscreen launch entry. Manual Fullscreen entries remain available."), Amaterasu.field("Recent games to import", recentGames, "Maximum number of recent games (0–10000); zero disables recent-game selection."), Amaterasu.field("Recent game age limit (days)", recentMaxAgeDays, "Ignore recent games older than this age (0–36500); zero disables the age cutoff."), Amaterasu.field("Included categories (JSON)", syncCategories, "Additional installed games whose category matches any listed value; an empty list adds no category matches."), Amaterasu.field("Excluded categories (JSON)", excludeCategories, "Games in these categories are excluded even if another setting would include them."), Amaterasu.field("Included plugin IDs (JSON)", syncPlugins, "Additional installed games from any listed Playnite plugin; an empty list adds no plugin matches."), Amaterasu.field("Excluded plugin IDs (JSON)", excludePlugins, "Games from these plugin IDs are excluded even if another setting would include them."), Amaterasu.field("Excluded game IDs (JSON)", excludeGames, "Game IDs to omit from every import selection."), runtimeStatus, saveStatus, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${settings.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, saveStatus, async () => {
                        playniteLoads.invalidate();
                        let saveStarted = false;
                        try {
                            const values = parsePlayniteSettings({
                                enabled: enabled.checked,
                                auto_sync: autoSync.checked,
                                sync_all_installed: syncAllInstalled.checked,
                                autosync_remove_uninstalled: removeUninstalled.checked,
                                autosync_delete_after_days: deleteAfterDays.valueAsNumber,
                                autosync_require_replacement: requireReplacement.checked,
                                focus_attempts: focusAttempts.valueAsNumber,
                                focus_timeout_secs: focusTimeoutSecs.valueAsNumber,
                                focus_exit_on_first: focusExitOnFirst.checked,
                                fullscreen_entry_enabled: fullscreenEntryEnabled.checked,
                                recent_games: recentGames.valueAsNumber,
                                recent_max_age_days: recentMaxAgeDays.valueAsNumber,
                                sync_categories: parsePlayniteFilterList(syncCategories.value, "Sync categories"),
                                exclude_categories: parsePlayniteFilterList(excludeCategories.value, "Excluded categories"),
                                sync_plugins: parsePlayniteFilterList(syncPlugins.value, "Sync plugin IDs"),
                                exclude_plugins: parsePlayniteFilterList(excludePlugins.value, "Excluded plugin IDs"),
                                exclude_games: parsePlayniteFilterList(excludeGames.value, "Excluded game IDs"),
                            });
                            saveStarted = true;
                            await savePlayniteSettings(context.api, settings, values);
                            await loadPlaynite();
                            context.notify("Playnite settings saved and confirmed. Runtime status is shown below.");
                        }
                        catch (error) {
                            if (error instanceof Amaterasu.ApiError && error.code === "revision_conflict") {
                                context.notify("Playnite settings changed elsewhere. The latest values have been reloaded.");
                                await loadPlaynite();
                                return;
                            }
                            if (saveStarted) {
                                await loadPlaynite();
                                context.notify("Playnite save could not be confirmed. Review the latest values before saving again.");
                                return;
                            }
                            throw error;
                        }
                    });
                });
                refreshStatus.className = "muted";
                playniteArea.replaceChildren(Amaterasu.panel("Playnite", form, Amaterasu.el("div", "actions", refresh, refreshStatus)));
            }
            catch (error) {
                if (context.content.isConnected)
                    playniteArea.replaceChildren(Amaterasu.errorPanel(error, loadPlaynite));
            }
        }
        async function loadPairing() {
            pairingArea.replaceChildren(Amaterasu.el("p", "muted", "Loading paired clients…"));
            try {
                const loaded = await pairingLoads.read(context.api);
                if (loaded === null)
                    return;
                const { settings, inputStatus } = loaded;
                if (!context.content.isConnected)
                    return;
                if (settings.clients.length === 0) {
                    pairingArea.replaceChildren(Amaterasu.panel("Paired client overrides", Amaterasu.empty("No paired clients", "Pair a Moonlight client before setting client-specific streaming values."), Amaterasu.badge(`Revision ${settings.revision}`)));
                    return;
                }
                const rows = settings.clients.map((client) => {
                    const fec = Amaterasu.input(`fec_${client.certificate_id}`, String(client.config_overrides?.fec_percentage ?? ""), "number");
                    fec.min = "1";
                    fec.max = "255";
                    fec.step = "1";
                    fec.placeholder = "Inherit";
                    fec.setAttribute("aria-label", `FEC percentage override for ${client.name}`);
                    const backButtonTimeout = Amaterasu.input(`back_button_timeout_${client.certificate_id}`, client.config_overrides?.back_button_timeout?.toString() ?? "", "number");
                    backButtonTimeout.min = String(-0x8000_0000);
                    backButtonTimeout.max = String(0x7fff_ffff);
                    backButtonTimeout.step = "1";
                    backButtonTimeout.placeholder = "Inherit";
                    backButtonTimeout.setAttribute("aria-label", `Back button timeout override for ${client.name}`);
                    const controller = sparseInputOverrideSelect("controller", client.config_overrides?.controller);
                    controller.setAttribute("aria-label", `Controller input override for ${client.name}`);
                    const gamepadChoices = inputGamepadProfileChoices(client.config_overrides?.gamepad ?? "", inputStatus?.supported_profiles ?? ["auto"], inputStatus?.recognized_profiles ?? ["auto", ...(client.config_overrides?.gamepad ? [client.config_overrides.gamepad] : [])]);
                    const gamepad = Amaterasu.el("select");
                    gamepad.setAttribute("aria-label", `Gamepad profile override for ${client.name}`);
                    const inheritGamepad = Amaterasu.el("option", "", "Inherit");
                    inheritGamepad.value = "";
                    gamepad.append(inheritGamepad);
                    for (const choice of gamepadChoices) {
                        const option = Amaterasu.el("option", "", choice.label);
                        option.value = choice.value;
                        gamepad.append(option);
                    }
                    gamepad.value = client.config_overrides?.gamepad ?? "";
                    const keyboard = sparseInputOverrideSelect("keyboard", client.config_overrides?.keyboard);
                    keyboard.setAttribute("aria-label", `Keyboard input override for ${client.name}`);
                    const mouse = sparseInputOverrideSelect("mouse", client.config_overrides?.mouse);
                    mouse.setAttribute("aria-label", `Mouse input override for ${client.name}`);
                    const nativePenTouch = sparseInputOverrideSelect("native_pen_touch", client.config_overrides?.native_pen_touch);
                    nativePenTouch.setAttribute("aria-label", `Native pen/touch preference override for ${client.name}`);
                    const streamAudio = Amaterasu.el("select");
                    streamAudio.setAttribute("aria-label", `Audio streaming override for ${client.name}`);
                    const audioChoices = [
                        ["", "Inherit"],
                        ["true", "Enabled"],
                        ["false", "Disabled"],
                    ];
                    for (const [value, label] of audioChoices) {
                        const option = Amaterasu.el("option", "", label);
                        option.value = value;
                        streamAudio.append(option);
                    }
                    streamAudio.value = client.config_overrides?.stream_audio === undefined
                        ? ""
                        : String(client.config_overrides.stream_audio);
                    const audioSinkMode = Amaterasu.el("select");
                    audioSinkMode.setAttribute("aria-label", `Audio endpoint override for ${client.name}`);
                    for (const [value, label] of [
                        ["inherit", "Inherit application/host"],
                        ["automatic", "Automatic/default endpoint"],
                        ["specific", "Specific endpoint ID"],
                    ]) {
                        const option = Amaterasu.el("option", "", label);
                        option.value = value ?? "";
                        audioSinkMode.append(option);
                    }
                    const currentAudioSink = client.config_overrides?.audio_sink;
                    audioSinkMode.value = currentAudioSink === undefined ? "inherit" : currentAudioSink === "" ? "automatic" : "specific";
                    const audioSinkId = Amaterasu.input(`audio_sink_${client.certificate_id}`, currentAudioSink ?? "");
                    audioSinkId.maxLength = 4096;
                    audioSinkId.placeholder = "Exact endpoint ID";
                    audioSinkId.setAttribute("aria-label", `Specific audio endpoint ID for ${client.name}`);
                    const updateAudioSink = () => { audioSinkId.disabled = audioSinkMode.value !== "specific"; };
                    audioSinkMode.addEventListener("change", updateAudioSink);
                    updateAudioSink();
                    const virtualSinkMode = Amaterasu.el("select");
                    virtualSinkMode.setAttribute("aria-label", `Virtual sink override for ${client.name}`);
                    for (const [value, label] of [
                        ["inherit", "Inherit application/host"],
                        ["automatic", "Clear inherited virtual sink"],
                        ["specific", "Specific virtual sink selector"],
                    ]) {
                        const option = Amaterasu.el("option", "", label);
                        option.value = value ?? "";
                        virtualSinkMode.append(option);
                    }
                    const currentVirtualSink = client.config_overrides?.virtual_sink;
                    virtualSinkMode.value = currentVirtualSink === undefined ? "inherit" : currentVirtualSink === "" ? "automatic" : "specific";
                    const virtualSinkSelector = Amaterasu.input(`virtual_sink_${client.certificate_id}`, currentVirtualSink ?? "");
                    virtualSinkSelector.maxLength = 4096;
                    virtualSinkSelector.placeholder = "Exact ID or device name";
                    virtualSinkSelector.setAttribute("aria-label", `Specific virtual sink selector for ${client.name}`);
                    const updateVirtualSink = () => { virtualSinkSelector.disabled = virtualSinkMode.value !== "specific"; };
                    virtualSinkMode.addEventListener("change", updateVirtualSink);
                    updateVirtualSink();
                    const captureOnly = Amaterasu.el("select");
                    captureOnly.setAttribute("aria-label", `Capture-only audio override for ${client.name}`);
                    for (const [value, label] of [
                        ["", "Inherit"],
                        ["true", "Capture without routing"],
                        ["false", "Allow default-output routing"],
                    ]) {
                        const option = Amaterasu.el("option", "", label);
                        option.value = value ?? "";
                        captureOnly.append(option);
                    }
                    captureOnly.value = client.config_overrides?.audio_sink_capture_only === undefined
                        ? ""
                        : String(client.config_overrides.audio_sink_capture_only);
                    const status = Amaterasu.el("div");
                    const unsupported = unsupportedPairingOverrideFields(client);
                    const save = Amaterasu.button("Save", () => Amaterasu.runAction(save, status, async () => {
                        pairingLoads.invalidate();
                        let saveStarted = false;
                        try {
                            const values = {
                                ...parseClientFecOverride(fec.value),
                                back_button_timeout: Amaterasu.parseBackButtonTimeoutOverride(backButtonTimeout.value),
                                stream_audio: Amaterasu.parseStreamAudioOverride(streamAudio.value),
                                audio_sink: Amaterasu.parseAudioSinkOverride(audioSinkMode.value, audioSinkId.value),
                                virtual_sink: Amaterasu.parseVirtualSinkOverride(virtualSinkMode.value, virtualSinkSelector.value),
                                audio_sink_capture_only: Amaterasu.parseAudioSinkCaptureOnlyOverride(captureOnly.value),
                                controller: Amaterasu.parseInputBooleanOverride(controller.value, "Controller input"),
                                gamepad: Amaterasu.parseGamepadProfileOverride(gamepad.value, inputStatus?.recognized_profiles ?? ["auto", ...(client.config_overrides?.gamepad ? [client.config_overrides.gamepad] : [])]),
                                keyboard: Amaterasu.parseInputBooleanOverride(keyboard.value, "Keyboard input"),
                                mouse: Amaterasu.parseInputBooleanOverride(mouse.value, "Mouse input"),
                                native_pen_touch: Amaterasu.parseInputBooleanOverride(nativePenTouch.value, "Native pen/touch preference"),
                            };
                            saveStarted = true;
                            await savePairingClientOverrides(context.api, settings, client.certificate_id, values);
                            await loadPairing();
                            context.notify(`Stored overrides confirmed for ${client.name}. Active stream application is not confirmed.`);
                        }
                        catch (error) {
                            if (saveStarted) {
                                await loadPairing();
                                context.notify(error instanceof Amaterasu.ApiError && error.code === "revision_conflict"
                                    ? "Paired clients changed elsewhere. Review the refreshed overrides before saving again."
                                    : "Paired client override save could not be confirmed. Review the refreshed values before saving again.");
                                return;
                            }
                            throw error;
                        }
                    }), "primary");
                    save.disabled = !context.api.allows("pairing.admin") || unsupported.length > 0;
                    if (unsupported.length > 0)
                        status.textContent = `Save unavailable: this client has stored overrides this editor cannot preserve (${unsupported.join(", ")}).`;
                    const identity = Amaterasu.el("div", "", Amaterasu.el("strong", "", client.name), Amaterasu.el("small", "muted", `${client.client_id} · certificate ${client.certificate_id.slice(0, 12)}…`));
                    return [identity, fec, backButtonTimeout, streamAudio, Amaterasu.el("div", "", audioSinkMode, audioSinkId), Amaterasu.el("div", "", virtualSinkMode, virtualSinkSelector), captureOnly, controller, gamepad, keyboard, mouse, nativePenTouch, Amaterasu.el("div", "", save, status)];
                });
                const tableView = Amaterasu.table(["Paired client", "FEC percentage", "Back button timeout (ms)", "Audio streaming", "Audio endpoint", "Virtual sink", "Capture only", "Controller", "Gamepad", "Keyboard", "Mouse", "Native pen/touch", ""], rows);
                pairingArea.replaceChildren(Amaterasu.panel("Paired client overrides", Amaterasu.el("p", "muted", "Blank inherits the application and host value. Back button timeout accepts signed 32-bit milliseconds, with -1 disabling the timeout. Recognized unavailable gamepad profiles stay labeled. Each save confirms the stored pairing revision; applying client input preferences during active streams is not yet implemented."), tableView, Amaterasu.el("div", "actions", Amaterasu.badge(`Revision ${settings.revision}`))));
            }
            catch (error) {
                if (context.content.isConnected)
                    pairingArea.replaceChildren(Amaterasu.errorPanel(error, loadPairing));
            }
        }
        async function loadHistorySettings() {
            historyArea.replaceChildren(Amaterasu.el("p", "muted", "Loading session history settings…"));
            try {
                const state = await historyLoads.read(context.api);
                if (state === null)
                    return;
                const { settings: reply, runtime } = state;
                if (!context.content.isConnected)
                    return;
                const form = Amaterasu.el("form", "settings-form");
                const enabled = Amaterasu.input("session_history_enabled", "", "checkbox");
                enabled.checked = reply.settings.enabled;
                const ttl = Amaterasu.input("session_history_ttl_days", String(reply.settings.ttl_days), "number");
                const size = Amaterasu.input("session_history_db_size_limit_mb", String(reply.settings.db_size_limit_mb), "number");
                for (const control of [ttl, size]) {
                    control.required = true;
                    control.min = "0";
                    control.max = String(maxHistoryLimit);
                    control.step = "1";
                }
                const ttlField = Amaterasu.field("Keep ended sessions for (days)", ttl, "Zero disables age pruning.");
                const sizeField = Amaterasu.field("Approximate live database limit (MiB)", size, "Zero disables size pruning. Active sessions are never deleted to meet this limit.");
                const status = Amaterasu.el("p", "muted", historySettingsRuntimeDescription(reply, runtime));
                const save = Amaterasu.el("button", "button primary", "Save history settings");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                form.append(Amaterasu.field("Record session history", enabled, "Recording changes wait for active streams to end and pending launches to resolve. Check the runtime status for the effective policy."), Amaterasu.el("p", "muted", "Retention limits are saved while recording is disabled. Stored sessions remain readable, but automatic pruning and deletion resume only after recording is enabled."), ttlField, sizeField, status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${reply.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, status, async () => {
                        historyLoads.invalidate();
                        const updated = await saveHistorySettingsState(context.api, reply, enabled.checked, ttl.valueAsNumber, size.valueAsNumber);
                        context.notify(historySettingsRuntimeDescription(updated.settings, updated.runtime));
                        await loadHistorySettings();
                    });
                });
                historyArea.replaceChildren(Amaterasu.panel("Session history", form));
            }
            catch (error) {
                if (context.content.isConnected)
                    historyArea.replaceChildren(Amaterasu.errorPanel(error, loadHistorySettings));
            }
        }
        async function loadInput() {
            inputArea.replaceChildren(Amaterasu.el("p", "muted", "Loading input policy…"));
            try {
                const loaded = await inputLoads.read(context.api);
                if (loaded === null)
                    return;
                const { settings, runtime: runtimeStatus } = loaded;
                if (!context.content.isConnected)
                    return;
                const form = Amaterasu.el("form", "settings-form");
                const keyboard = Amaterasu.input("input_keyboard", "", "checkbox");
                keyboard.checked = settings.values.keyboard;
                const mouse = Amaterasu.input("input_mouse", "", "checkbox");
                mouse.checked = settings.values.mouse;
                const controller = Amaterasu.input("input_controller", "", "checkbox");
                controller.checked = settings.values.controller;
                const ds4BackAsTouchpadClick = Amaterasu.input("input_ds4_back_as_touchpad_click", "", "checkbox");
                ds4BackAsTouchpadClick.checked = settings.values.ds4_back_as_touchpad_click;
                const nativePenTouch = Amaterasu.input("input_native_pen_touch", "", "checkbox");
                nativePenTouch.checked = settings.values.native_pen_touch;
                const highResolutionScrolling = Amaterasu.input("input_high_resolution_scrolling", "", "checkbox");
                highResolutionScrolling.checked = settings.values.high_resolution_scrolling;
                const alwaysSendScancodes = Amaterasu.input("input_always_send_scancodes", "", "checkbox");
                alwaysSendScancodes.checked = settings.values.always_send_scancodes;
                const keyRightAltToKeyWin = Amaterasu.input("input_key_rightalt_to_key_win", "", "checkbox");
                keyRightAltToKeyWin.checked = settings.values.key_rightalt_to_key_win;
                const keyRepeatDelay = Amaterasu.input("input_key_repeat_delay", String(settings.values.key_repeat_delay), "number");
                keyRepeatDelay.required = true;
                keyRepeatDelay.min = "0";
                keyRepeatDelay.max = String(maxInputKeyRepeatDelayMs);
                keyRepeatDelay.step = "1";
                const keyRepeatFrequency = Amaterasu.input("input_key_repeat_frequency", String(settings.values.key_repeat_frequency), "number");
                keyRepeatFrequency.required = true;
                keyRepeatFrequency.min = "0";
                keyRepeatFrequency.step = "any";
                const backButtonTimeout = Amaterasu.input("input_back_button_timeout", String(settings.values.back_button_timeout), "number");
                backButtonTimeout.required = true;
                backButtonTimeout.min = String(minInputBackButtonTimeoutMs);
                backButtonTimeout.max = String(maxInputBackButtonTimeoutMs);
                backButtonTimeout.step = "1";
                const gamepadProfile = Amaterasu.el("select");
                gamepadProfile.name = "input_gamepad_profile";
                const gamepadProfileChoices = inputGamepadProfileChoices(settings.values.gamepad, runtimeStatus?.supported_profiles ?? ["auto"], runtimeStatus?.recognized_profiles ?? [settings.values.gamepad]);
                for (const choice of gamepadProfileChoices) {
                    const option = Amaterasu.el("option", "", choice.label);
                    option.value = choice.value;
                    gamepadProfile.append(option);
                }
                gamepadProfile.value = settings.values.gamepad;
                const selectedGamepadUnavailable = settings.values.controller &&
                    (runtimeStatus !== null &&
                        (!runtimeStatus.supported_profiles.includes(settings.values.gamepad) ||
                            (runtimeStatus.lease !== null &&
                                runtimeStatus.gamepad === settings.values.gamepad &&
                                !runtimeStatus.controller_input)));
                const gamepadStatus = Amaterasu.el("p", selectedGamepadUnavailable ? "error" : "muted", inputGamepadStatusDescription(settings.values.gamepad, runtimeStatus));
                const status = Amaterasu.el("p", settings.applied && runtimeStatus !== null && !runtimeStatus.controller_cleanup_required
                    ? "muted" : "error", inputPolicyRuntimeDescription(settings, runtimeStatus));
                const save = Amaterasu.el("button", "button primary", "Save input policy");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                form.append(Amaterasu.field("Keyboard input", keyboard, "Disabling keyboard input suppresses key events and releases held keys immediately."), Amaterasu.field("Mouse input", mouse, "Disabling mouse input suppresses pointer, button, and scroll events and releases held buttons."), Amaterasu.field("Controller input", controller, "Disabling controller input suppresses controller activity and removes owned virtual gamepad slots. Keyboard and mouse input continue."), Amaterasu.field("DS4 Back as touchpad click", ds4BackAsTouchpadClick, "For the DS4 profile, maps Back to a touchpad click when that backend supports it. The preference stays saved if the selected backend cannot apply it."), Amaterasu.field("Native pen and touch policy", nativePenTouch, "Windows-only global preference. Native pen/touch event transport and platform support are not implemented; saving this value does not activate pen or touch events."), Amaterasu.field("High-resolution scrolling", highResolutionScrolling, "Preserves raw wheel units for supported streaming clients. Disable to convert scroll input to legacy 120-unit detents."), Amaterasu.field("Always send scan codes", alwaysSendScancodes, "On Windows, maps non-normalized Moonlight key packets to scan codes. Linux persists this preference but uses its fixed key mapping."), Amaterasu.field("Map Right Alt to Left Windows", keyRightAltToKeyWin, "Maps Moonlight Right Alt (VK_RMENU) to Left Windows (VK_LWIN) on Windows and Linux; a held key stays paired with its original release if this changes."), Amaterasu.field("Key repeat delay (milliseconds)", keyRepeatDelay, "Default is 500 milliseconds. Set to 0 to disable key repeat."), Amaterasu.field("Key repeat frequency (Hz)", keyRepeatFrequency, "Default is 24.9 Hz. Set to 0 to use the 24.9 Hz default; decimal frequencies are supported."), Amaterasu.field("Back button timeout (milliseconds)", backButtonTimeout, "On Linux and Windows, when controller input is available, holding Back for this many milliseconds pulses Guide for 100 milliseconds. Any negative value disables it; other platforms do not inject gamepads."), Amaterasu.field("Gamepad profile", gamepadProfile, "Profiles listed by input status are supported by this backend. A saved platform-recognized profile outside that list remains visible and is labeled unavailable."), gamepadStatus, status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${settings.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, status, async () => {
                        inputLoads.invalidate();
                        let saveStarted = false;
                        try {
                            const values = {
                                ...parseInputPolicy(keyboard.checked, mouse.checked, highResolutionScrolling.checked, alwaysSendScancodes.checked, keyRightAltToKeyWin.checked, keyRepeatDelay.valueAsNumber, keyRepeatFrequency.valueAsNumber, backButtonTimeout.valueAsNumber, gamepadProfile.value, gamepadProfileChoices.map((choice) => choice.value), controller.checked, nativePenTouch.checked),
                                ds4_back_as_touchpad_click: ds4BackAsTouchpadClick.checked,
                            };
                            saveStarted = true;
                            const saved = await saveInputPolicySettings(context.api, settings, values);
                            await loadInput();
                            context.notify(saved.applied && !saved.cleanup_required
                                ? "Input policy saved and confirmed. Runtime status is shown below."
                                : "Input policy saved and confirmed; releasing held native input is pending.");
                        }
                        catch (error) {
                            if (saveStarted) {
                                await loadInput();
                                context.notify(error instanceof Amaterasu.ApiError && error.code === "revision_conflict"
                                    ? "Input policy changed elsewhere. Review the latest values before saving again."
                                    : "Input policy save could not be confirmed. Review the latest values before saving again.");
                                return;
                            }
                            throw error;
                        }
                    });
                });
                inputArea.replaceChildren(Amaterasu.panel("Input policy", form));
            }
            catch (error) {
                if (context.content.isConnected)
                    inputArea.replaceChildren(Amaterasu.errorPanel(error, loadInput));
            }
        }
        async function loadSteamRuntimeStatus(target, context, settings, generation) {
            try {
                const status = await context.api.post("steam/status");
                if (!target.isConnected || !steamLoads.isCurrent(generation))
                    return;
                target.textContent = status.settings_ready && status.enabled !== settings.values.enabled
                    ? "Saved Steam discovery setting is not yet reflected in provider status. Refresh to check application."
                    : steamStatusDescription(status);
            }
            catch (error) {
                if (!target.isConnected || !steamLoads.isCurrent(generation))
                    return;
                target.textContent =
                    error instanceof Amaterasu.ApiError && error.code === "forbidden"
                        ? "Runtime status requires steam.read; settings remain available with configuration access."
                        : "Current provider status is unavailable; settings remain editable.";
            }
        }
        async function loadHost() {
            hostArea.replaceChildren(Amaterasu.el("p", "muted", "Loading host name…"));
            let generation = null;
            try {
                const loaded = await hostLoads.read(context.api);
                if (loaded === null)
                    return;
                const { settings } = loaded;
                generation = loaded.generation;
                if (!context.content.isConnected)
                    return;
                const form = Amaterasu.el("form", "settings-form");
                const hostname = Amaterasu.input("hostname", settings.configured_hostname);
                hostname.maxLength = 128;
                const port = Amaterasu.input("port", String(settings.port), "number");
                port.required = true;
                port.min = "1029";
                port.max = "65514";
                port.step = "1";
                const addressFamily = Amaterasu.el("select");
                addressFamily.name = "address_family";
                const familyOptions = [
                    ["startup", `Use startup policy (effective: ${settings.address_family})`],
                    ["ipv4", "IPv4 only"],
                    ["both", "IPv4 and IPv6"],
                ];
                for (const [value, label] of familyOptions) {
                    const option = Amaterasu.el("option", "", label);
                    option.value = value;
                    addressFamily.append(option);
                }
                addressFamily.value = settings.configured_address_family ?? "startup";
                const useStartupBindAddress = Amaterasu.input("use_startup_bind_address", "", "checkbox");
                useStartupBindAddress.checked = settings.configured_bind_address === null;
                const bindAddress = Amaterasu.input("bind_address", settings.configured_bind_address ?? "");
                bindAddress.disabled = useStartupBindAddress.checked;
                useStartupBindAddress.addEventListener("change", () => {
                    bindAddress.disabled = useStartupBindAddress.checked;
                });
                const capture = Amaterasu.el("select");
                capture.name = "capture";
                const captureChoices = [
                    ["auto", "Automatic (startup capture backend)"],
                    ["ddx", "Desktop Duplication"],
                    ["wgc", "Windows Graphics Capture"],
                    ["wgcc", "Windows Graphics Capture, constant rate"],
                ];
                for (const [value, label] of captureChoices) {
                    const option = Amaterasu.el("option", "", label);
                    option.value = value;
                    capture.append(option);
                }
                capture.value = settings.configured_capture;
                const outputName = Amaterasu.el("select");
                outputName.name = "output_name";
                const automaticOutput = Amaterasu.el("option", "", "Automatic (startup output)");
                automaticOutput.value = "";
                outputName.append(automaticOutput);
                let outputDiscoveryIssue = null;
                if (settings.capture_provider && context.api.allows("media.read")) {
                    try {
                        const discovered = await context.api.post(`media/${settings.capture_provider}_outputs`);
                        if (!context.content.isConnected || !hostLoads.isCurrent(generation))
                            return;
                        for (const output of discovered.outputs) {
                            try {
                                if (!output.attached || !output.id || parseOutputNameSelection(output.id) !== output.id)
                                    continue;
                                const option = Amaterasu.el("option", "", `${output.device_name} (${output.id})`);
                                option.value = output.id;
                                outputName.append(option);
                            }
                            catch {
                            }
                        }
                    }
                    catch {
                        outputDiscoveryIssue = "Capture outputs could not be listed; the saved selection remains available.";
                    }
                }
                else if (!settings.capture_provider) {
                    outputDiscoveryIssue = "This host has no supported capture provider for output selection.";
                }
                else {
                    outputDiscoveryIssue = "Listing capture outputs requires media.read.";
                }
                if (settings.configured_output_name && !Array.from(outputName.options).some((option) => option.value === settings.configured_output_name)) {
                    const saved = Amaterasu.el("option", "", `Saved output: ${settings.configured_output_name}`);
                    saved.value = settings.configured_output_name;
                    outputName.append(saved);
                }
                outputName.value = settings.configured_output_name ?? "";
                const streamAudio = Amaterasu.input("stream_audio", "", "checkbox");
                streamAudio.checked = settings.stream_audio;
                const wgcPacing = Amaterasu.el("select");
                wgcPacing.name = "wgc_pacing_smoothing";
                for (const [value, label] of [
                    ["", "Default (enabled)"],
                    ["true", "Enabled"],
                    ["false", "Disabled"],
                ]) {
                    const option = Amaterasu.el("option", "", label);
                    option.value = value ?? "";
                    wgcPacing.append(option);
                }
                wgcPacing.value = settings.configured_wgc_pacing_smoothing === null
                    ? ""
                    : String(settings.configured_wgc_pacing_smoothing);
                const audioSink = Amaterasu.el("select");
                audioSink.name = "audio_sink";
                const automaticAudioSink = Amaterasu.el("option", "", "Automatic (startup audio endpoint)");
                automaticAudioSink.value = "";
                audioSink.append(automaticAudioSink);
                let audioDiscoveryIssue = null;
                if (context.api.allows("media.read")) {
                    try {
                        const discovered = await context.api.post("media/audio_endpoints");
                        if (!context.content.isConnected || !hostLoads.isCurrent(generation))
                            return;
                        for (const endpoint of discovered.endpoints) {
                            try {
                                if (!endpoint.id || parseAudioSinkSelection(endpoint.id) !== endpoint.id)
                                    continue;
                                const option = Amaterasu.el("option", "", `${endpoint.name}${endpoint.default_multimedia ? " (default)" : ""} (${endpoint.id})`);
                                option.value = endpoint.id;
                                audioSink.append(option);
                            }
                            catch {
                            }
                        }
                    }
                    catch {
                        audioDiscoveryIssue = "Audio endpoints could not be listed; the saved selection remains available.";
                    }
                }
                else {
                    audioDiscoveryIssue = "Listing audio endpoints requires media.read.";
                }
                if (settings.configured_audio_sink && !Array.from(audioSink.options).some((option) => option.value === settings.configured_audio_sink)) {
                    const saved = Amaterasu.el("option", "", `Saved endpoint: ${settings.configured_audio_sink}`);
                    saved.value = settings.configured_audio_sink;
                    audioSink.append(saved);
                }
                audioSink.value = settings.configured_audio_sink ?? "";
                const virtualSinkEnabled = Amaterasu.input("virtual_sink_enabled", "", "checkbox");
                virtualSinkEnabled.checked = settings.configured_virtual_sink !== null;
                const virtualSink = Amaterasu.input("virtual_sink", settings.configured_virtual_sink ?? "");
                virtualSink.maxLength = 4096;
                virtualSink.placeholder = "Exact device ID or name";
                const updateVirtualSink = () => { virtualSink.disabled = !virtualSinkEnabled.checked; };
                virtualSinkEnabled.addEventListener("change", updateVirtualSink);
                updateVirtualSink();
                const globalPreparation = Amaterasu.el("textarea");
                globalPreparation.name = "global_preparation";
                globalPreparation.rows = 10;
                globalPreparation.value = JSON.stringify(settings.configured_global_preparation, null, 2);
                const globalPreparationValidation = Amaterasu.el("p", "muted");
                const updateGlobalPreparationValidation = () => {
                    try {
                        const steps = Amaterasu.parsePreparationSteps(globalPreparation.value);
                        globalPreparation.setCustomValidity("");
                        globalPreparationValidation.className = "muted";
                        globalPreparationValidation.textContent =
                            "Valid preparation list with " +
                                steps.length +
                                " step" +
                                (steps.length === 1 ? "" : "s") +
                                ".";
                    }
                    catch (error) {
                        const message = Amaterasu.messageOf(error);
                        globalPreparation.setCustomValidity(message);
                        globalPreparationValidation.className = "error";
                        globalPreparationValidation.textContent = message;
                    }
                };
                globalPreparation.addEventListener("input", updateGlobalPreparationValidation);
                updateGlobalPreparationValidation();
                const audioSinkCaptureOnly = Amaterasu.input("audio_sink_capture_only", "", "checkbox");
                audioSinkCaptureOnly.checked = settings.audio_sink_capture_only;
                const externalIp = Amaterasu.input("external_ip", settings.configured_external_ip ?? "");
                externalIp.maxLength = 4096;
                const videoBatchSize = Amaterasu.input("video_max_batch_size_kb", settings.configured_video_max_batch_size_kb === null
                    ? ""
                    : String(settings.configured_video_max_batch_size_kb), "number");
                videoBatchSize.min = "0";
                videoBatchSize.max = "64";
                videoBatchSize.step = "1";
                videoBatchSize.placeholder = "Default (64)";
                const maxBitrate = Amaterasu.input("max_bitrate", settings.configured_max_bitrate === null
                    ? ""
                    : String(settings.configured_max_bitrate));
                maxBitrate.placeholder = "No host cap";
                const minimumFpsTarget = Amaterasu.input("minimum_fps_target", settings.configured_minimum_fps_target === null
                    ? ""
                    : String(settings.configured_minimum_fps_target), "number");
                minimumFpsTarget.min = "0";
                minimumFpsTarget.max = "1000";
                minimumFpsTarget.step = "any";
                minimumFpsTarget.placeholder = "Default (20)";
                const pingTimeout = Amaterasu.input("ping_timeout_ms", settings.configured_ping_timeout_ms === null
                    ? ""
                    : String(settings.configured_ping_timeout_ms), "number");
                pingTimeout.min = "0";
                pingTimeout.max = "2147483647";
                pingTimeout.step = "1";
                pingTimeout.placeholder = "Default (10000)";
                const status = Amaterasu.el("div");
                const save = Amaterasu.el("button", "button primary", "Save host settings");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                form.append(Amaterasu.field("Name shown to Moonlight", hostname, "Leave blank to use this PC's name."), Amaterasu.field("Moonlight HTTP port", port, "HTTPS uses the port five below this value."), Amaterasu.field("Address family", addressFamily, "Choose IPv4 only, both families, or retain the startup policy."), Amaterasu.field("Use startup bind address", useStartupBindAddress, `The effective bind address is ${settings.bind_address}. Turn this off to choose an explicit address; a blank value is distinct from startup policy.`), Amaterasu.field("Bind address override", bindAddress, "Use a literal IPv4 or IPv6 address. An explicit blank uses the startup policy's wildcard/loopback behavior."), Amaterasu.field("Capture backend", capture, `Automatic uses the startup backend. The current backend is ${settings.capture_provider ?? "unavailable"}; explicit Windows modes fail if unavailable. Save and reopen settings to list outputs for a new backend.`), Amaterasu.field("Capture output", outputName, "Choose a display for future Moonlight streams. Automatic retains the configured startup output."), Amaterasu.field("Stream audio", streamAudio, "Enable host audio capture for future streams. Applications and paired clients may override this setting."), Amaterasu.field("WGC pacing smoothing", wgcPacing, `Preserve the frame pacing grid after eligible Windows WGC capture misses. Current effective policy: ${settings.effective_wgc_pacing_smoothing ? "enabled" : "disabled"}. Constant-rate WGC currently lacks fresh-frame identity for this policy.`), Amaterasu.field("Audio capture endpoint", audioSink, `Choose the output device used for loopback capture in future streams. Automatic uses the startup endpoint. Current effective endpoint: ${settings.effective_audio_sink}.`), Amaterasu.field("Set a virtual audio sink", virtualSinkEnabled, "Keep the selector unset to use the regular capture endpoint. An explicitly empty selector is preserved as distinct from unset; Amaterasu currently cannot perform Sunshine's host-audio fallback selection for it."), Amaterasu.field("Virtual sink ID or name", virtualSink, "Match a Windows virtual output by ID, friendly name, device description, or adapter name. The selected virtual sink takes precedence over the regular endpoint for future streams."), Amaterasu.field("Global preparation commands (JSON)", globalPreparation, "Enter an ordered JSON array of up to 8 run commands with optional undo commands. Executables and working directories must be absolute native paths; blank input clears the list."), globalPreparationValidation, Amaterasu.field("Capture selected audio endpoint without changing default outputs", audioSinkCaptureOnly, "For an explicitly selected Windows endpoint, keep the host's Console, Multimedia, and Communications default outputs. The setting applies to future streams."), Amaterasu.field("External IP", externalIp, "Stored for configuration parity. This host does not currently advertise or detect an external IP from this setting; blank resets it."), Amaterasu.field("Video batch size (KiB)", videoBatchSize, `Use 16, 32, or 64 KiB; blank or 0 selects 64 KiB. Current effective size: ${settings.effective_video_max_batch_size_kb} KiB${settings.video_batch_size_fallback ? " (configured value fell back to 64 KiB)" : ""}.`), Amaterasu.field("Maximum bitrate (kbps)", maxBitrate, `A positive value caps the host's initial stream bitrate. Blank resets to no host cap; zero or negative values also leave it uncapped. Caps below 100 kbps cannot launch with the current encoder. Current cap: ${settings.effective_max_bitrate_kbps === null ? "none" : `${settings.effective_max_bitrate_kbps} kbps`}.`), Amaterasu.field("Minimum frame rate (FPS)", minimumFpsTarget, `On idle capture, repeat the last accepted frame to target this rate. Zero uses the negotiated stream FPS; blank resets to 20 FPS. Current effective target: ${settings.effective_minimum_fps_target} FPS.`), Amaterasu.field("Stream ping timeout (ms)", pingTimeout, `Disconnect a stream after this much time without a ping. Zero expires immediately; blank resets to the default. Current effective timeout: ${settings.effective_ping_timeout_ms} ms.`), ...(outputDiscoveryIssue ? [Amaterasu.el("p", "muted", outputDiscoveryIssue)] : []), ...(audioDiscoveryIssue ? [Amaterasu.el("p", "muted", audioDiscoveryIssue)] : []), Amaterasu.el("p", "muted", `Active HTTP addresses: ${settings.http_addresses?.join(", ") || "off"} · HTTPS: ${settings.https_addresses?.join(", ") || "off"} · RTSP: ${settings.rtsp_addresses?.join(", ") || "off"}`), Amaterasu.el("p", "muted", `Active ports: HTTP ${settings.active_http_port ?? "off"}, HTTPS ${settings.active_https_port ?? "off"}, RTSP ${settings.active_rtsp_port ?? "off"}`), ...(settings.listener_error ? [Amaterasu.el("p", "error", settings.listener_error)] : []), ...(settings.rtsp_listener_error ? [Amaterasu.el("p", "error", settings.rtsp_listener_error)] : []), status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${settings.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    if (!form.reportValidity())
                        return;
                    void Amaterasu.runAction(save, status, async () => {
                        const network = parseNvhttpNetworkOverrides(addressFamily.value, useStartupBindAddress.checked, bindAddress.value);
                        const values = {
                            hostname: hostname.value,
                            port: port.valueAsNumber,
                            ...network,
                            capture: parseCaptureSelection(capture.value),
                            output_name: parseOutputNameSelection(outputName.value),
                            stream_audio: streamAudio.checked,
                            video_max_batch_size_kb: parseVideoMaxBatchSize(videoBatchSize.value),
                            max_bitrate: parseMaxBitrate(maxBitrate.value),
                            minimum_fps_target: parseMinimumFpsTarget(minimumFpsTarget.value),
                            ping_timeout_ms: parsePingTimeout(pingTimeout.value),
                            audio_sink: parseAudioSinkSelection(audioSink.value),
                            virtual_sink: parseVirtualSinkSelection(virtualSinkEnabled.checked, virtualSink.value),
                            audio_sink_capture_only: audioSinkCaptureOnly.checked,
                            external_ip: parseExternalIp(externalIp.value),
                            wgc_pacing_smoothing: parseWgcPacingSmoothing(wgcPacing.value),
                            global_preparation: Amaterasu.parsePreparationSteps(globalPreparation.value),
                        };
                        hostLoads.invalidate();
                        try {
                            await saveNvhttpHostSettings(context.api, settings, values);
                            context.notify("Host settings saved and read back. Future-stream options apply to new streams; check active listener status below.");
                            await loadHost();
                        }
                        catch (error) {
                            if (error instanceof Amaterasu.ApiError && await recoverNvhttpSaveFailure(error.code, loadHost, (message) => context.notify(message))) {
                                return;
                            }
                            throw error;
                        }
                    });
                });
                if (hostLoads.isCurrent(generation))
                    hostArea.replaceChildren(Amaterasu.panel("Moonlight host settings", form));
            }
            catch (error) {
                if (context.content.isConnected && (generation === null || hostLoads.isCurrent(generation)))
                    hostArea.replaceChildren(Amaterasu.errorPanel(error, loadHost));
            }
        }
        async function load() {
            area.replaceChildren(Amaterasu.el("p", "muted", "Loading settings…"));
            try {
                const document = await registeredLoads.read(context.api);
                if (document === null)
                    return;
                if (!context.content.isConnected)
                    return;
                const form = Amaterasu.el("form", "settings-form");
                const inputs = new Map();
                for (const [name, definition] of Object.entries(document.fields)) {
                    const value = document.values[name] ?? definition.default;
                    let control;
                    if (definition.type === "enum") {
                        const select = Amaterasu.el("select");
                        select.name = name;
                        for (const optionValue of definition.options) {
                            const option = Amaterasu.el("option", "", optionValue);
                            option.value = optionValue;
                            select.append(option);
                        }
                        select.value = String(value);
                        control = select;
                    }
                    else if (definition.type === "list" ||
                        definition.type === "object") {
                        const editor = Amaterasu.el("textarea");
                        editor.name = name;
                        editor.rows = 6;
                        editor.value = JSON.stringify(value, null, 2);
                        control = editor;
                    }
                    else {
                        control = Amaterasu.input(name, String(value), definition.type === "boolean"
                            ? "checkbox"
                            : definition.type === "integer" ||
                                definition.type === "decimal"
                                ? "number"
                                : "text");
                    }
                    if (definition.type === "boolean")
                        control.checked = Boolean(value);
                    if (definition.type === "integer" || definition.type === "decimal") {
                        const numericControl = control;
                        numericControl.step = definition.type === "integer" ? "1" : "any";
                        if (definition.minimum != null)
                            numericControl.min = String(definition.minimum);
                        if (definition.maximum != null)
                            numericControl.max = String(definition.maximum);
                    }
                    if (definition.type === "string") {
                        const textControl = control;
                        if (definition.min_length != null)
                            textControl.minLength = definition.min_length;
                        if (definition.max_length != null)
                            textControl.maxLength = definition.max_length;
                    }
                    inputs.set(name, control);
                    form.append(Amaterasu.field(name, control, `Type: ${definition.type}`));
                }
                const status = Amaterasu.el("div");
                const save = Amaterasu.el("button", "button primary", "Save settings");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                form.append(Amaterasu.el("p", "muted", "Saving confirms stored values. Runtime use of registered settings is not verified on this page."), status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${document.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, status, async () => {
                        const values = {};
                        for (const [name, control] of inputs) {
                            const definition = document.fields[name];
                            if (definition)
                                values[name] = Amaterasu.fieldValue(definition, control);
                        }
                        registeredLoads.invalidate();
                        try {
                            await saveRegisteredConfiguration(context.api, document, values);
                            context.notify("Registered settings saved and read back. Runtime application is not verified here.");
                            await load();
                        }
                        catch (error) {
                            if (error instanceof Amaterasu.ApiError && error.code === "revision_conflict") {
                                await load();
                                context.notify("Registered settings changed elsewhere. Review the latest values and save again.");
                                return;
                            }
                            throw error;
                        }
                    });
                });
                area.replaceChildren(Object.keys(document.fields).length
                    ? Amaterasu.panel("Registered values", form)
                    : Amaterasu.empty("No settings fields", "The registered schema is empty. Schema setup and migration are available through the management API."));
            }
            catch (error) {
                if (!context.content.isConnected)
                    return;
                area.replaceChildren(error instanceof Amaterasu.ApiError && error.code === "not_registered"
                    ? Amaterasu.empty("No settings schema yet", "Register a schema for the management principal through the authenticated configuration API. This screen will then show its editable values.")
                    : Amaterasu.errorPanel(error, load));
            }
        }
        void load();
        void loadAuth();
        void loadHostLogging();
        void loadHost();
        void loadSteam();
        void loadLutris();
        void loadPlaynite();
        void loadInput();
        void loadHistorySettings();
        void loadPairing();
    }
    Amaterasu.renderSettings = renderSettings;
})(Amaterasu || (Amaterasu = {}));
var Amaterasu;
(function (Amaterasu) {
    function parseRepositoryList(value) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Repository response is invalid.");
        const reply = value;
        if (reply.ok !== true || !Array.isArray(reply.repositories))
            throw new Error("Repository state is unavailable.");
        return reply.repositories.map((item) => {
            if (!item || typeof item !== "object" || Array.isArray(item))
                throw new Error("Repository entry is invalid.");
            const repository = item;
            if (typeof repository.id !== "string" ||
                typeof repository.location !== "string" ||
                typeof repository.fingerprint !== "string" ||
                typeof repository.revoked !== "boolean" ||
                !Array.isArray(repository.candidates))
                throw new Error("Repository entry is invalid.");
            const candidates = repository.candidates.map((raw) => {
                if (!raw || typeof raw !== "object" || Array.isArray(raw))
                    throw new Error("Repository choice is invalid.");
                const choice = raw;
                if (typeof choice.slot !== "string" ||
                    typeof choice.implementation !== "string" ||
                    typeof choice.version !== "string" ||
                    typeof choice.manifest_sha256 !== "string" ||
                    !["native_set", "web_bundle"].includes(choice.kind))
                    throw new Error("Repository choice is invalid.");
                return choice;
            });
            return { ...repository, candidates };
        });
    }
    Amaterasu.parseRepositoryList = parseRepositoryList;
    function parseRepositoryPage(value) {
        const repositories = parseRepositoryList(value);
        const reply = value;
        const nextOffset = reply.next_offset;
        if (nextOffset !== null &&
            (!Number.isSafeInteger(nextOffset) || nextOffset <= 0 || nextOffset > 4096))
            throw new Error("Repository pagination is invalid.");
        return { repositories, nextOffset: nextOffset };
    }
    Amaterasu.parseRepositoryPage = parseRepositoryPage;
    function renderUpdates(context) {
        const area = Amaterasu.el("div");
        context.content.append(Amaterasu.sectionHeading("Modules & UI", "Choose verified implementations from approved publishers.", Amaterasu.button("Refresh", load)), Amaterasu.el("div", "notice", Amaterasu.el("strong", "", "Publisher authority is broad"), Amaterasu.el("p", "", "Approving a publisher allows it to supply any native module slot, including management, and the UI. Native modules share their worker's user privileges, while a replacement UI runs in the signed-in browser session and can use its API authority. Approval does not grant elevation. A signature proves source, not safe behavior.")), area);
        async function load() {
            try {
                const byId = new Map();
                let offset = 0;
                while (true) {
                    const page = parseRepositoryPage(await context.api.post("repositories/list", { offset }));
                    for (const repository of page.repositories) {
                        const previous = byId.get(repository.id);
                        if (previous && (previous.fingerprint !== repository.fingerprint || previous.revoked !== repository.revoked))
                            throw new Error("Repository state changed during listing. Refresh to retry.");
                        byId.set(repository.id, previous
                            ? { ...previous, candidates: [...previous.candidates, ...repository.candidates] }
                            : repository);
                    }
                    if (page.nextOffset === null)
                        break;
                    if (page.nextOffset <= offset)
                        throw new Error("Repository pagination did not advance.");
                    offset = page.nextOffset;
                }
                if (!area.isConnected)
                    return;
                renderChoices([...byId.values()]);
            }
            catch (error) {
                if (area.isConnected)
                    area.replaceChildren(Amaterasu.errorPanel(error, load));
            }
        }
        function renderChoices(repositories) {
            const active = repositories.filter((repository) => !repository.revoked);
            const repositorySelect = Amaterasu.el("select");
            const slotSelect = Amaterasu.el("select");
            const implementationSelect = Amaterasu.el("select");
            const details = Amaterasu.el("div", "notice");
            const choices = Amaterasu.panel("Choose an implementation", Amaterasu.field("Approved repository", repositorySelect), Amaterasu.field("Module slot or UI", slotSelect), Amaterasu.field("Implementation and version", implementationSelect), details);
            const addOption = (select, value, label) => {
                const option = Amaterasu.el("option", "", label);
                option.value = value;
                select.append(option);
            };
            active.forEach((repository, index) => addOption(repositorySelect, String(index), `${repository.id} · ${repository.fingerprint.slice(0, 16)}…`));
            const selectedRepository = () => active[Number(repositorySelect.value)];
            const selectedChoice = () => {
                const repository = selectedRepository();
                return repository?.candidates.filter((choice) => choice.slot === slotSelect.value)[Number(implementationSelect.value)];
            };
            const showChoice = () => {
                const repository = selectedRepository();
                const choice = selectedChoice();
                if (!repository || !choice) {
                    details.replaceChildren(Amaterasu.el("p", "muted", "No verified choice is available for this slot."));
                    return;
                }
                const command = choice.kind === "web_bundle"
                    ? `amaterasu-release repository-install-ui ROOT ${repository.id} ${choice.implementation} ${choice.version} OUTPUT`
                    : `amaterasu-release repository-download ROOT ${repository.id} ${choice.slot} ${choice.implementation} ${choice.version} OUTPUT`;
                details.replaceChildren(Amaterasu.el("strong", "", `${choice.implementation} ${choice.version}`), Amaterasu.el("p", "", `Source: ${repository.location}`), Amaterasu.el("p", "", `Publisher key SHA-256: ${repository.fingerprint}`), Amaterasu.el("p", "", `Signed manifest SHA-256: ${choice.manifest_sha256}`), Amaterasu.el("p", "", choice.kind === "web_bundle"
                    ? "Run this explicit administrator command to download, verify and select the UI. The previous UI remains recoverable at /recovery."
                    : "This choice supplies a complete native component set. Download and stage it for review. A separate activate command explicitly authorizes its native code to run under the user account at the next development worker start or probe. Production native activation remains disabled."), Amaterasu.el("code", "", command), ...(choice.kind === "native_set" ? [
                    Amaterasu.el("code", "", `amaterasu-release repository-stage-native ROOT ${repository.id} ${choice.slot} ${choice.implementation} ${choice.version} OUTPUT`),
                    Amaterasu.el("code", "", "amaterasu-release repository-activate-native ROOT STAGED_DIGEST"),
                ] : []));
            };
            const fillImplementations = () => {
                implementationSelect.replaceChildren();
                selectedRepository()?.candidates
                    .filter((choice) => choice.slot === slotSelect.value)
                    .forEach((choice, index) => addOption(implementationSelect, String(index), `${choice.implementation} · ${choice.version}`));
                showChoice();
            };
            const fillSlots = () => {
                slotSelect.replaceChildren();
                const slots = [...new Set(selectedRepository()?.candidates.map((choice) => choice.slot) ?? [])];
                slots.forEach((slot) => addOption(slotSelect, slot, slot === "amaterasu.ui" ? "UI" : slot));
                fillImplementations();
            };
            repositorySelect.addEventListener("change", fillSlots);
            slotSelect.addEventListener("change", fillImplementations);
            implementationSelect.addEventListener("change", showChoice);
            fillSlots();
            area.replaceChildren(Amaterasu.panel("Approve a publisher", Amaterasu.el("p", "", "On this development host, use repository-approve with the publisher's public key and a SHA-256 fingerprint confirmed through an independent channel. The repository URL alone cannot establish key identity."), Amaterasu.el("code", "", "amaterasu-release repository-approve ROOT ID HTTPS_URL PUBLIC_KEY_HEX CONFIRMED_SHA256"), Amaterasu.el("p", "", "Then run repository-refresh ROOT ID to retrieve the signed catalog. Key rotation uses a new publisher ID; revoke the old ID explicitly.")), choices, Amaterasu.panel("Recovery", Amaterasu.el("p", "", "The host shell can restore retained verified UI or native sets without loading the selected management module. Use `amaterasu web-recover ROOT DIGEST` or `amaterasu native-recover ROOT [DIGEST]`. Release floors remain intact. The management /recovery page also covers UI failures while management is healthy.")));
        }
        void load();
    }
    Amaterasu.renderUpdates = renderUpdates;
})(Amaterasu || (Amaterasu = {}));
var Amaterasu;
(function (Amaterasu) {
    Amaterasu.LOCALE_OPTIONS = [
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
    ];
    function isRecord(value) {
        return typeof value === "object" && value !== null && !Array.isArray(value);
    }
    function localeFieldDefinition() {
        return {
            type: "enum",
            default: "en",
            options: [...Amaterasu.LOCALE_OPTIONS],
        };
    }
    function validateLocale(value) {
        if (typeof value !== "string" ||
            !Amaterasu.LOCALE_OPTIONS.includes(value)) {
            throw new Error("The locale value is not one of the supported tokens.");
        }
        return value;
    }
    Amaterasu.validateLocale = validateLocale;
    function validateLocaleDefinition(definition) {
        if (!isRecord(definition) || definition.type !== "enum") {
            throw new Error("The registered locale field must be an enum.");
        }
        if (definition.default !== "en") {
            throw new Error("The registered locale field must default to en.");
        }
        const options = definition.options;
        if (!Array.isArray(options) ||
            options.length !== Amaterasu.LOCALE_OPTIONS.length ||
            !Amaterasu.LOCALE_OPTIONS.every((locale, index) => options[index] === locale)) {
            throw new Error("The registered locale options are incompatible.");
        }
    }
    Amaterasu.validateLocaleDefinition = validateLocaleDefinition;
    function validateConfigurationDocument(configuration) {
        if (!isRecord(configuration) ||
            !Number.isSafeInteger(configuration.revision) ||
            !isRecord(configuration.fields) ||
            (configuration.values !== undefined && !isRecord(configuration.values))) {
            throw new Error("The configuration service returned an invalid document.");
        }
        return configuration;
    }
    function localeFromConfiguration(configuration) {
        const document = validateConfigurationDocument(configuration);
        if (!Object.prototype.hasOwnProperty.call(document.fields, "locale")) {
            throw new Error("The registered configuration has no locale field.");
        }
        const definition = document.fields.locale;
        validateLocaleDefinition(definition);
        const values = document.values ?? {};
        const value = Object.prototype.hasOwnProperty.call(values, "locale")
            ? values.locale
            : definition.default;
        return validateLocale(value);
    }
    Amaterasu.localeFromConfiguration = localeFromConfiguration;
    function extendLocaleConfiguration(configuration) {
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
    Amaterasu.extendLocaleConfiguration = extendLocaleConfiguration;
    function documentLanguage(value) {
        return validateLocale(value).replaceAll("_", "-");
    }
    Amaterasu.documentLanguage = documentLanguage;
    function errorMessage(error) {
        return error instanceof Error ? error.message : String(error);
    }
    function errorCode(error) {
        if (!isRecord(error) || typeof error.code !== "string")
            return null;
        return error.code;
    }
    async function getLocaleConfiguration(api) {
        try {
            return validateConfigurationDocument(await api.post("configuration/get"));
        }
        catch (error) {
            if (errorCode(error) !== "not_registered")
                throw error;
            if (!api.allows("configuration.write")) {
                throw new Error("Locale setup requires configuration.write access.");
            }
            await api.post("configuration/register", {
                fields: { locale: localeFieldDefinition() },
            });
            return validateConfigurationDocument(await api.post("configuration/get"));
        }
    }
    async function ensureLocaleConfiguration(api) {
        if (!api.allows("configuration.read")) {
            throw new Error("Locale preferences require configuration.read access.");
        }
        let configuration = await getLocaleConfiguration(api);
        if (!Object.prototype.hasOwnProperty.call(configuration.fields, "locale")) {
            if (!api.allows("configuration.write")) {
                throw new Error("Adding the locale preference requires configuration.write access.");
            }
            const extended = extendLocaleConfiguration(configuration);
            await api.post("configuration/migrate", {
                fields: extended.fields,
                expected_revision: configuration.revision,
            });
            configuration = validateConfigurationDocument(await api.post("configuration/get"));
        }
        localeFromConfiguration(configuration);
        return configuration;
    }
    function mountLocaleSelector(api) {
        const container = document.createElement("div");
        container.className = "locale-control";
        const label = document.createElement("label");
        label.htmlFor = "global-locale";
        label.textContent = "Locale";
        label.title = "Stored locale preference. Console text remains English.";
        const select = document.createElement("select");
        select.id = "global-locale";
        select.setAttribute("aria-label", "Locale");
        for (const locale of Amaterasu.LOCALE_OPTIONS) {
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
        let revision = null;
        const updateDisabledState = () => {
            select.disabled = busy || !canWrite || !loaded;
        };
        const showError = (message) => {
            status.textContent = "";
            error.textContent = message;
        };
        const clearError = () => {
            error.textContent = "";
        };
        const load = async (previousError) => {
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
                if (previousError)
                    showError(previousError);
                else if (!canWrite)
                    status.textContent = "Read only";
                else
                    status.textContent = "";
            }
            catch (loadError) {
                loaded = false;
                revision = null;
                showError(previousError
                    ? `${previousError} Reload failed: ${errorMessage(loadError)}`
                    : `Unable to load locale preference: ${errorMessage(loadError)}`);
            }
            finally {
                busy = false;
                updateDisabledState();
            }
        };
        select.addEventListener("change", () => {
            if (!loaded || !canWrite || revision === null)
                return;
            let locale;
            try {
                locale = validateLocale(select.value);
            }
            catch (validationError) {
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
                .catch((saveError) => load(`Unable to save locale preference: ${errorMessage(saveError)}`));
        });
        if (!api.allows("configuration.read")) {
            showError("Locale preferences require configuration.read access.");
        }
        else {
            void load();
        }
        updateDisabledState();
        return container;
    }
    Amaterasu.mountLocaleSelector = mountLocaleSelector;
})(Amaterasu || (Amaterasu = {}));
var Amaterasu;
(function (Amaterasu) {
    function renderOverview(context) {
        const area = Amaterasu.el("div");
        context.content.append(Amaterasu.sectionHeading("Host overview", "Your applications, sessions, and local services in one place.", Amaterasu.button("Refresh", load)), area);
        async function load() {
            const snapshots = await Promise.allSettled([
                context.api.post("catalog/list", { limit: 32 }),
                context.api.post("session/list", { limit: 16 }),
                context.api.post("application/list", { limit: 16 }),
                context.api.post("steam/status"),
            ]);
            if (!context.content.isConnected)
                return;
            const [catalog, sessions, applications, steam] = snapshots;
            const metrics = Amaterasu.el("div", "metrics");
            const addMetric = (label, value, detail) => metrics.append(Amaterasu.el("div", "metric", Amaterasu.el("span", "muted", label), Amaterasu.el("strong", "", value), Amaterasu.el("small", "muted", detail)));
            addMetric("Catalog applications", catalog?.status === "fulfilled"
                ? Amaterasu.countLabel(catalog.value.entries.length, catalog.value.next_cursor)
                : "—", "Manual library entries");
            addMetric("Sessions", sessions?.status === "fulfilled"
                ? Amaterasu.countLabel(sessions.value.sessions.length, sessions.value.next_offset)
                : "—", "Current host instance");
            addMetric("Tracked processes", applications?.status === "fulfilled"
                ? Amaterasu.countLabel(applications.value.processes.length, applications.value.next_offset)
                : "—", "Running and recently exited");
            addMetric("Steam discoveries", steam?.status === "fulfilled" ? String(steam.value.game_count) : "—", steam?.status === "fulfilled" && steam.value.scanning
                ? "Scanning local libraries"
                : "Latest provider scan");
            const hero = Amaterasu.el("section", "overview-hero", Amaterasu.el("div", "", Amaterasu.badge("YOUR HOST, YOUR LIBRARY", "accent"), Amaterasu.el("h2", "", "A home for your applications."), Amaterasu.el("p", "", "Curate your library and manage the services that keep this host running."), Amaterasu.button("Open your library →", () => context.navigate("catalog"), "primary")), Amaterasu.el("div", "orbit-art", Amaterasu.el("span", "orbit one"), Amaterasu.el("span", "orbit two"), Amaterasu.el("span", "orbit-core", "A")));
            const serviceRows = snapshots.map((snapshot, index) => [
                Amaterasu.el("strong", "", ["Catalog", "Sessions", "Applications", "Steam provider"][index] ??
                    ""),
                snapshot.status === "fulfilled"
                    ? Amaterasu.badge("Available", "good")
                    : Amaterasu.badge("Unavailable"),
                snapshot.status === "rejected"
                    ? Amaterasu.messageOf(snapshot.reason)
                    : "Responded to the latest refresh",
            ]);
            area.replaceChildren(hero, metrics, Amaterasu.panel("Service access", Amaterasu.table(["Service", "Status", "Details"], serviceRows)), Amaterasu.el("div", "notice", Amaterasu.el("strong", "", "Streaming is still being built"), Amaterasu.el("p", "", "Review pending device pairing from Host & devices. Capture controls and a browser streaming view are not available in this release.")));
        }
        void load();
    }
    Amaterasu.renderOverview = renderOverview;
    function parseHistoryRuntimeStatus(value) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("History status reply is invalid.");
        const reply = value;
        if (reply.ok !== true || !reply.status || typeof reply.status !== "object" || Array.isArray(reply.status))
            throw new Error("History status reply is invalid.");
        const status = reply.status;
        if (status.applied_revision !== null &&
            (!Number.isSafeInteger(status.applied_revision) || status.applied_revision < 0))
            throw new Error("History applied revision is invalid.");
        const prune = status.last_prune;
        if (!prune || typeof prune !== "object" || Array.isArray(prune))
            throw new Error("History retention status is invalid.");
        const retention = prune;
        for (const key of ["available", "configuration_ready", "enabled", "effective_enabled", "degraded"])
            if (typeof status[key] !== "boolean")
                throw new Error(`History ${key} status is invalid.`);
        for (const key of ["failed_operations", "active_streams", "sessions", "samples", "events", "live_bytes"])
            if (!Number.isSafeInteger(status[key]) || status[key] < 0)
                throw new Error(`History ${key} count is invalid.`);
        if (status.pending_activities !== undefined &&
            (!Number.isSafeInteger(status.pending_activities) || status.pending_activities < 0))
            throw new Error("History pending activities count is invalid.");
        for (const key of ["sessions_deleted", "live_bytes", "size_limit_bytes"])
            if (!Number.isSafeInteger(retention[key]) || retention[key] < 0)
                throw new Error(`History retention ${key} count is invalid.`);
        if (typeof retention.size_limit_unmet !== "boolean")
            throw new Error("History retention limit status is invalid.");
        return { ...status, pending_activities: status.pending_activities ?? 0 };
    }
    Amaterasu.parseHistoryRuntimeStatus = parseHistoryRuntimeStatus;
    function historyRecordingPolicyDescription(status) {
        if (!status.configuration_ready)
            return "History configuration is not ready; recording is not confirmed.";
        if (status.effective_enabled !== status.enabled) {
            const action = status.enabled ? "enable" : "disable";
            const effective = status.effective_enabled ? "enabled" : "disabled";
            if (status.pending_activities > 0) {
                const pending = status.pending_activities === 1
                    ? "1 pending launch resolves"
                    : `${status.pending_activities} pending launches resolve`;
                const active = status.active_streams > 0
                    ? `${status.active_streams} active ${status.active_streams === 1 ? "stream ends" : "streams end"} and `
                    : "";
                return `Recording ${action} is deferred until ${active}${pending}; the effective policy remains ${effective}.`;
            }
            return `Recording ${action} is deferred until active streams end; new streams still use the ${effective} policy.`;
        }
        const effective = status.effective_enabled ? "enabled" : "disabled";
        return status.pending_activities > 0
            ? `Recording is ${effective} under the current policy; pending launches have not joined yet.`
            : `Recording is ${effective} for new streams.`;
    }
    Amaterasu.historyRecordingPolicyDescription = historyRecordingPolicyDescription;
    function historyActivityDescription(status) {
        const active = `${status.active_streams} active ${status.active_streams === 1 ? "stream" : "streams"}`;
        const pending = `${status.pending_activities} pending ${status.pending_activities === 1 ? "launch" : "launches"}`;
        return `${active} · ${pending}`;
    }
    Amaterasu.historyActivityDescription = historyActivityDescription;
    function historyRuntimeDescription(status) {
        if (!status.available)
            return ["History storage is unavailable; recording and recorded session reads cannot run."];
        const lines = [status.applied_revision === null
                ? "Runtime configuration revision is not confirmed."
                : `Runtime applied configuration revision ${status.applied_revision}.`,
            historyRecordingPolicyDescription(status)];
        lines.push(`${historyActivityDescription(status)} · ${status.sessions} stored sessions · ${status.samples} samples · ${status.events} events · ${status.live_bytes} live bytes.`);
        if (status.configuration_ready && !status.effective_enabled)
            lines.push("Stored sessions remain readable. Automatic retention and deletion are paused until recording is enabled.");
        lines.push(`Last retention pass deleted ${status.last_prune.sessions_deleted} sessions; ${status.last_prune.live_bytes} live bytes against ${status.last_prune.size_limit_bytes === 0 ? "no size limit" : `${status.last_prune.size_limit_bytes} byte limit`}.`);
        if (status.last_prune.size_limit_unmet)
            lines.push("The retention size limit remains unmet; active sessions are preserved.");
        if (status.failed_operations > 0)
            lines.push(`${status.failed_operations} history storage operations have failed since this service started.`);
        if (status.degraded && status.failed_operations === 0 && !status.last_prune.size_limit_unmet && status.configuration_ready)
            lines.push("History reports degraded operation.");
        return lines;
    }
    Amaterasu.historyRuntimeDescription = historyRuntimeDescription;
    function parseHistoryDetailReply(value, sessionId) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("History detail reply is invalid.");
        const reply = value;
        if (reply.ok !== true || !reply.detail || typeof reply.detail !== "object" || Array.isArray(reply.detail))
            throw new Error("History detail reply is invalid.");
        const detail = reply.detail;
        const session = detail.session;
        if (!session || typeof session !== "object" || Array.isArray(session) ||
            session.session_id !== sessionId)
            throw new Error("History detail belongs to a different session.");
        for (const key of ["sample_count", "event_count"])
            if (!Number.isSafeInteger(detail[key]) || detail[key] < 0)
                throw new Error(`History ${key} is invalid.`);
        for (const key of ["samples_truncated", "events_truncated"])
            if (typeof detail[key] !== "boolean")
                throw new Error(`History ${key} is invalid.`);
        for (const [key, countKey] of [["samples", "sample_count"], ["events", "event_count"]]) {
            const rows = detail[key];
            if (!Array.isArray(rows) || rows.length > detail[countKey] || rows.some((row) => !row || typeof row !== "object" || Array.isArray(row) || row.session_id !== sessionId ||
                !Number.isSafeInteger(row.recorded_at_unix)))
                throw new Error(`History ${key} are invalid.`);
        }
        for (const sample of detail.samples) {
            for (const key of ["frames_sent", "bytes_sent", "dropped_frames", "actual_fps", "actual_bitrate_kbps"])
                if (typeof sample[key] !== "number" || !Number.isFinite(sample[key]) || sample[key] < 0)
                    throw new Error(`History sample ${key} is invalid.`);
        }
        if (detail.events.some((event) => typeof event.event_type !== "string"))
            throw new Error("History event type is invalid.");
        return reply;
    }
    Amaterasu.parseHistoryDetailReply = parseHistoryDetailReply;
    function parseHistoryListReply(value, offset) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("History session page is invalid.");
        const reply = value;
        if (reply.ok !== true || !Array.isArray(reply.sessions) || reply.sessions.length > 25 ||
            (reply.next_offset !== null && (!Number.isSafeInteger(reply.next_offset) ||
                reply.next_offset <= offset)))
            throw new Error("History session page is invalid.");
        for (const entry of reply.sessions) {
            if (!entry || typeof entry !== "object" || Array.isArray(entry))
                throw new Error("History session row is invalid.");
            const session = entry;
            if (typeof session.session_id !== "string" || session.session_id.length === 0 ||
                new TextEncoder().encode(session.session_id).length > 128 ||
                /[\u0000-\u001f\u007f-\u009f]/u.test(session.session_id) ||
                ["app_name", "client_name", "protocol"].some((key) => typeof session[key] !== "string") ||
                !Number.isSafeInteger(session.started_at_unix) ||
                (session.ended_at_unix !== null && !Number.isSafeInteger(session.ended_at_unix)) ||
                (session.duration_seconds !== null &&
                    (!Number.isSafeInteger(session.duration_seconds) || session.duration_seconds < 0)))
                throw new Error("History session row is invalid.");
        }
        return reply;
    }
    Amaterasu.parseHistoryListReply = parseHistoryListReply;
    function parseHistoryDeleteReply(value) {
        if (!value || typeof value !== "object" || Array.isArray(value) ||
            value.ok !== true)
            throw new Error("History deletion was not confirmed. Refresh the recorded sessions before trying again.");
    }
    Amaterasu.parseHistoryDeleteReply = parseHistoryDeleteReply;
    async function confirmHistoryDeletion(api, sessionId) {
        try {
            await api.post("history/session_detail", { session_id: sessionId });
        }
        catch (error) {
            if (error && typeof error === "object" &&
                error.code === "not_found")
                return;
            throw error;
        }
        throw new Error("History deletion could not be confirmed by readback. Refresh the recorded sessions.");
    }
    Amaterasu.confirmHistoryDeletion = confirmHistoryDeletion;
    function renderSessions(context) {
        const area = Amaterasu.el("div");
        const status = Amaterasu.el("div");
        const historyRuntimeArea = Amaterasu.el("div");
        const savedArea = Amaterasu.el("div");
        const savedDetail = Amaterasu.el("div");
        const savedStatus = Amaterasu.el("div");
        let offset = 0;
        const history = [0];
        let savedOffset = 0;
        const savedHistory = [0];
        let savedListRequest = 0;
        let savedDetailRequest = 0;
        let historyStatusRequest = 0;
        let historyDeleteAvailable = false;
        let historyDeleteReason = "Checking history status before deletion is available.";
        let deleteControls = [];
        const deleteNotice = Amaterasu.el("p", "muted", historyDeleteReason);
        let selectedSavedSession = null;
        context.content.append(Amaterasu.sectionHeading("Sessions", "Inspect and stop sessions owned by this management console.", Amaterasu.button("Refresh", () => {
            void load();
            void loadHistoryRuntime();
            void loadSaved();
        })), status, area, Amaterasu.panel("History runtime status", historyRuntimeArea), Amaterasu.panel("Recorded stream sessions", savedArea, savedStatus, savedDetail));
        function applyHistoryDeleteAvailability() {
            deleteNotice.textContent = historyDeleteReason;
            for (const control of deleteControls)
                control.button.disabled = !control.ended || !context.api.allows("session.write") || !historyDeleteAvailable;
        }
        async function loadHistoryRuntime() {
            const request = ++historyStatusRequest;
            historyDeleteAvailable = false;
            historyDeleteReason = "Checking history status before deletion is available.";
            applyHistoryDeleteAvailability();
            historyRuntimeArea.replaceChildren(Amaterasu.el("p", "muted", "Loading history runtime status…"));
            try {
                const runtime = parseHistoryRuntimeStatus(await context.api.post("history/history_status"));
                if (!context.content.isConnected || request !== historyStatusRequest)
                    return false;
                historyDeleteAvailable = runtime.available && runtime.configuration_ready && runtime.effective_enabled;
                historyDeleteReason = historyDeleteAvailable
                    ? ""
                    : runtime.available && runtime.configuration_ready && !runtime.effective_enabled
                        ? "Deletion is unavailable while history recording is disabled. Stored sessions remain readable."
                        : "Deletion is unavailable until history storage and configuration are ready.";
                applyHistoryDeleteAvailability();
                historyRuntimeArea.replaceChildren(...historyRuntimeDescription(runtime).map((line) => Amaterasu.el("p", "muted", line)), Amaterasu.button("Refresh history status", () => { void loadHistoryRuntime(); }));
                return historyDeleteAvailable;
            }
            catch (error) {
                if (context.content.isConnected && request === historyStatusRequest) {
                    historyDeleteReason = "Deletion is unavailable until history runtime status can be confirmed. Stored sessions remain readable.";
                    applyHistoryDeleteAvailability();
                    historyRuntimeArea.replaceChildren(Amaterasu.errorPanel(error, () => { void loadHistoryRuntime(); }));
                }
                return false;
            }
        }
        async function load() {
            try {
                const reply = await context.api.post("session/list", {
                    offset,
                    limit: 16,
                });
                if (!context.content.isConnected)
                    return;
                const rows = reply.sessions.map((session) => {
                    const stop = Amaterasu.button(session.state === "stopping" ? "Finish stop" : "Stop session", async () => {
                        if (!(await Amaterasu.confirmAction("Stop this session?", `Session “${session.key}” will be moved toward its stopped state.`, session.state === "stopping" ? "Finish stop" : "Stop session")))
                            return;
                        await Amaterasu.runAction(stop, status, async () => {
                            await context.api.post(`session/${session.state === "stopping" ? "finish_stop" : "stop"}`, { session_id: session.id });
                            context.notify("Session updated.");
                            await load();
                        });
                    });
                    stop.disabled =
                        session.state === "stopped" || !context.api.allows("session.write");
                    return [
                        Amaterasu.el("div", "", Amaterasu.el("strong", "", session.key), Amaterasu.el("small", "muted block", `Session ${session.id}`)),
                        Amaterasu.badge(session.state, session.state === "active" ? "good" : ""),
                        String(session.lease_count ?? Object.keys(session.leases ?? {}).length),
                        stop,
                    ];
                });
                area.replaceChildren(reply.sessions.length
                    ? Amaterasu.table(["Session", "State", "Leases", "Action"], rows)
                    : Amaterasu.empty("No sessions yet", "Sessions created through the management API appear here. Streaming clients and pairing are not available yet."), Amaterasu.pager(history.length > 1
                    ? () => {
                        history.pop();
                        offset = history[history.length - 1] ?? 0;
                        void load();
                    }
                    : null, reply.next_offset != null
                    ? () => {
                        offset = reply.next_offset;
                        history.push(offset);
                        void load();
                    }
                    : null, `Showing from ${offset + 1}`));
            }
            catch (error) {
                if (context.content.isConnected)
                    area.replaceChildren(Amaterasu.errorPanel(error, load));
            }
        }
        async function loadSaved() {
            const request = ++savedListRequest;
            savedArea.replaceChildren(Amaterasu.el("p", "muted", "Loading recorded sessions…"));
            try {
                const reply = parseHistoryListReply(await context.api.post("history/list_sessions", { limit: 25, offset: savedOffset }), savedOffset);
                if (!context.content.isConnected || request !== savedListRequest)
                    return;
                const currentDeleteControls = [];
                const rows = reply.sessions.map((session) => {
                    const details = Amaterasu.button("Details", () => loadSavedDetail(session.session_id));
                    const remove = Amaterasu.button("Delete", async () => {
                        if (!context.api.allows("session.write") || !historyDeleteAvailable || !(await loadHistoryRuntime()))
                            return;
                        if (!(await Amaterasu.confirmAction("Delete this recorded session?", `The history for ${session.app_name || session.session_id} will be removed.`, "Delete history")))
                            return;
                        if (!context.api.allows("session.write") || !historyDeleteAvailable || !(await loadHistoryRuntime()))
                            return;
                        await Amaterasu.runAction(remove, savedStatus, async () => {
                            parseHistoryDeleteReply(await context.api.post("history/delete_session", {
                                session_id: session.session_id,
                            }));
                            await confirmHistoryDeletion(context.api, session.session_id);
                            if (selectedSavedSession === session.session_id) {
                                ++savedDetailRequest;
                                selectedSavedSession = null;
                                savedDetail.replaceChildren();
                            }
                            context.notify("Recorded session deleted.");
                            await loadSaved();
                        });
                        await loadHistoryRuntime();
                    }, "quiet danger-text");
                    currentDeleteControls.push({ button: remove, ended: session.ended_at_unix != null });
                    return [
                        Amaterasu.el("div", "", Amaterasu.el("strong", "", session.app_name || session.session_id), Amaterasu.el("small", "muted block", session.client_name || session.protocol)),
                        new Date(session.started_at_unix * 1000).toLocaleString(),
                        session.ended_at_unix == null ? Amaterasu.badge("Active", "good") :
                            `${session.duration_seconds ?? 0} seconds`,
                        Amaterasu.el("div", "actions", details, remove),
                    ];
                });
                deleteControls = currentDeleteControls;
                applyHistoryDeleteAvailability();
                savedArea.replaceChildren(deleteNotice, reply.sessions.length
                    ? Amaterasu.table(["Application / client", "Started", "Duration", "Actions"], rows)
                    : Amaterasu.empty("No recorded streams", "Completed streams appear here when history recording is enabled."), Amaterasu.pager(savedHistory.length > 1
                    ? () => {
                        savedHistory.pop();
                        savedOffset = savedHistory[savedHistory.length - 1] ?? 0;
                        void loadSaved();
                    }
                    : null, reply.next_offset != null
                    ? () => {
                        savedOffset = reply.next_offset;
                        savedHistory.push(savedOffset);
                        void loadSaved();
                    }
                    : null, `Showing from ${savedOffset + 1}`));
            }
            catch (error) {
                if (context.content.isConnected && request === savedListRequest)
                    savedArea.replaceChildren(Amaterasu.errorPanel(error, loadSaved));
            }
        }
        async function loadSavedDetail(sessionId) {
            selectedSavedSession = sessionId;
            const request = ++savedDetailRequest;
            savedDetail.replaceChildren(Amaterasu.el("p", "muted", "Loading session details…"));
            try {
                const reply = parseHistoryDetailReply(await context.api.post("history/session_detail", { session_id: sessionId }), sessionId);
                if (!context.content.isConnected || request !== savedDetailRequest)
                    return;
                const detail = reply.detail;
                if (!detail)
                    return;
                savedDetail.replaceChildren(Amaterasu.panel(`History for ${detail.session.app_name || detail.session.session_id}`, Amaterasu.el("p", "muted", `${detail.session.protocol} · ${detail.session.width}×${detail.session.height} · ${detail.session.target_fps} FPS · ${detail.session.codec || "codec unavailable"}`), Amaterasu.el("p", "muted", `${detail.sample_count} samples${detail.samples_truncated ? " (recent samples shown)" : ""} · ${detail.event_count} events${detail.events_truncated ? " (recent events shown)" : ""}`), detail.samples.length
                    ? Amaterasu.table(["Sample time", "Frames", "Bytes", "Dropped", "FPS", "Bitrate (kbps)"], detail.samples.map((sample) => [
                        new Date(sample.recorded_at_unix * 1000).toLocaleString(),
                        String(sample.frames_sent),
                        String(sample.bytes_sent),
                        String(sample.dropped_frames),
                        sample.actual_fps.toFixed(1),
                        sample.actual_bitrate_kbps.toFixed(1),
                    ]))
                    : Amaterasu.el("p", "muted", "No samples recorded."), detail.events.length
                    ? Amaterasu.table(["Event time", "Event"], detail.events.map((event) => [
                        new Date(event.recorded_at_unix * 1000).toLocaleString(),
                        event.event_type,
                    ]))
                    : Amaterasu.el("p", "muted", "No events recorded.")));
            }
            catch (error) {
                if (context.content.isConnected && request === savedDetailRequest)
                    savedDetail.replaceChildren(Amaterasu.errorPanel(error, () => loadSavedDetail(sessionId)));
            }
        }
        void load();
        void loadHistoryRuntime();
        void loadSaved();
    }
    Amaterasu.renderSessions = renderSessions;
    function renderApplications(context) {
        const area = Amaterasu.el("div");
        const status = Amaterasu.el("div");
        let offset = 0;
        const history = [0];
        context.content.append(Amaterasu.sectionHeading("Applications", "Processes launched through this management console.", Amaterasu.button("Refresh", load)), status, area);
        async function load() {
            try {
                const reply = await context.api.post("application/list", {
                    offset,
                    limit: 16,
                });
                if (!context.content.isConnected)
                    return;
                const rows = reply.processes.map((process) => {
                    const terminate = Amaterasu.button("Terminate", async () => {
                        if (!(await Amaterasu.confirmAction("Terminate this application?", `Stop process ${process.pid ?? process.process_id} and its owned process tree. Unsaved work may be lost.`, "Terminate")))
                            return;
                        await Amaterasu.runAction(terminate, status, async () => {
                            await context.api.post("application/terminate", {
                                process_id: process.process_id,
                            });
                            context.notify("Termination requested. Refresh to check its status.");
                            await load();
                        });
                    }, "quiet danger-text");
                    terminate.disabled =
                        process.state !== "running" ||
                            !context.api.allows("application.terminate");
                    return [
                        Amaterasu.el("strong", "", process.operation_id),
                        String(process.pid ?? "—"),
                        Amaterasu.badge(process.state, process.state === "running" ? "good" : ""),
                        process.exit_code != null ? String(process.exit_code) : "—",
                        terminate,
                    ];
                });
                area.replaceChildren(reply.processes.length
                    ? Amaterasu.table([
                        "Application operation",
                        "PID",
                        "State",
                        "Exit code",
                        "Action",
                    ], rows)
                    : Amaterasu.empty("No tracked processes", "Applications launched through the management API appear here. Catalog entries are metadata and do not launch automatically."), Amaterasu.pager(history.length > 1
                    ? () => {
                        history.pop();
                        offset = history[history.length - 1] ?? 0;
                        void load();
                    }
                    : null, reply.next_offset != null
                    ? () => {
                        offset = reply.next_offset;
                        history.push(offset);
                        void load();
                    }
                    : null, `Showing from ${offset + 1}`));
            }
            catch (error) {
                if (context.content.isConnected)
                    area.replaceChildren(Amaterasu.errorPanel(error, load));
            }
        }
        void load();
    }
    Amaterasu.renderApplications = renderApplications;
    function steamForceSyncOutcome(provider) {
        if (provider.enabled === false)
            return "failure";
        if (provider.force_sync_pending !== false ||
            provider.catalog_sync?.force_sync_pending !== false) {
            return "pending";
        }
        return provider.complete && provider.catalog_sync?.last_error === null
            ? "success"
            : "failure";
    }
    Amaterasu.steamForceSyncOutcome = steamForceSyncOutcome;
    function renderSteam(context) {
        const area = Amaterasu.el("div");
        const status = Amaterasu.el("div");
        let revision;
        let after;
        let providerStatus;
        let forceSyncPending = false;
        let forceSyncPollTimer;
        let forceSyncPollGeneration = 0;
        let forceSyncControl;
        const history = [undefined];
        function resetPagination() {
            revision = undefined;
            after = undefined;
            history.splice(0, history.length, undefined);
        }
        const scan = Amaterasu.button("Scan libraries", () => Amaterasu.runAction(scan, status, async () => {
            await context.api.post("steam/refresh");
            resetPagination();
            context.notify("Steam scan requested. Refresh to see completed discoveries.");
            await load();
        }), "primary");
        function updateForceSyncControl() {
            if (!forceSyncControl)
                return;
            const allowed = context.api.allows("steam.refresh");
            forceSyncControl.hidden = !allowed;
            forceSyncControl.disabled =
                !allowed ||
                    providerStatus?.enabled === false ||
                    providerStatus?.settings_ready === false ||
                    forceSyncPending;
        }
        function stopForceSyncPolling() {
            forceSyncPollGeneration += 1;
            if (forceSyncPollTimer !== undefined) {
                window.clearTimeout(forceSyncPollTimer);
                forceSyncPollTimer = undefined;
            }
        }
        function stopUnobservableForceSync() {
            if (!forceSyncPending)
                return;
            forceSyncPending = false;
            stopForceSyncPolling();
            updateForceSyncControl();
            const message = "Force catalog sync was accepted, but this session lacks steam.read and cannot check its status.";
            status.replaceChildren(Amaterasu.el("p", "notice warning", message));
            context.notify(message);
        }
        function finishForceSync(provider) {
            const outcome = steamForceSyncOutcome(provider);
            if (outcome === "pending")
                return;
            forceSyncPending = false;
            stopForceSyncPolling();
            updateForceSyncControl();
            if (outcome === "success") {
                status.replaceChildren(Amaterasu.el("p", "notice good", "Steam discovery completed and catalog sync succeeded."));
                context.notify("Steam discovery completed and catalog sync succeeded.");
                return;
            }
            const reason = provider.catalog_sync?.last_error;
            const message = reason
                ? `Steam catalog sync did not complete: ${reason}`
                : provider.enabled === false
                    ? "Steam discovery was disabled; no catalog sync was confirmed."
                    : provider.complete
                        ? "Steam catalog sync ended without a confirmed successful result."
                        : "Steam discovery was incomplete; no catalog sync was confirmed.";
            status.replaceChildren(Amaterasu.el("p", "notice warning", message));
            context.notify(message);
        }
        async function pollForceSync(generation) {
            if (!context.content.isConnected) {
                stopForceSyncPolling();
                return;
            }
            if (!context.api.allows("steam.read")) {
                stopUnobservableForceSync();
                return;
            }
            if (generation !== forceSyncPollGeneration ||
                !forceSyncPending ||
                !context.content.isConnected) {
                return;
            }
            try {
                const provider = await context.api.post("steam/status");
                if (generation !== forceSyncPollGeneration ||
                    !forceSyncPending ||
                    !context.content.isConnected) {
                    return;
                }
                providerStatus = provider;
                finishForceSync(provider);
                updateForceSyncControl();
                if (!forceSyncPending && context.content.isConnected) {
                    resetPagination();
                    await load(false);
                }
            }
            catch (error) {
                if (generation !== forceSyncPollGeneration ||
                    !forceSyncPending ||
                    !context.content.isConnected) {
                    return;
                }
                if (!context.api.allows("steam.read")) {
                    stopUnobservableForceSync();
                    return;
                }
                status.replaceChildren(Amaterasu.errorPanel(error));
            }
            if (generation === forceSyncPollGeneration &&
                forceSyncPending &&
                context.content.isConnected) {
                forceSyncPollTimer = window.setTimeout(() => {
                    forceSyncPollTimer = undefined;
                    void pollForceSync(generation);
                }, 1000);
            }
        }
        function startForceSyncPolling() {
            if (!forceSyncPending || !context.content.isConnected)
                return;
            if (!context.api.allows("steam.read")) {
                stopUnobservableForceSync();
                return;
            }
            stopForceSyncPolling();
            const generation = forceSyncPollGeneration;
            forceSyncPollTimer = window.setTimeout(() => {
                forceSyncPollTimer = undefined;
                void pollForceSync(generation);
            }, 1000);
        }
        async function requestForceSync() {
            const target = forceSyncControl;
            if (!target)
                return;
            await Amaterasu.runAction(target, status, async () => {
                const reply = await context.api.post("steam/force_sync");
                if (reply.ok !== true ||
                    reply.accepted !== true ||
                    reply.settings_ready !== true ||
                    reply.force_sync_pending !== true ||
                    reply.catalog_sync?.force_sync_pending !== true) {
                    throw new Error(reply.code
                        ? `Force catalog sync was not accepted: ${reply.code}`
                        : "Force catalog sync was not accepted.");
                }
                forceSyncPending = true;
                status.replaceChildren(Amaterasu.el("p", "notice", "Force catalog sync accepted. Waiting for a complete Steam scan."));
                updateForceSyncControl();
                if (!context.content.isConnected)
                    return;
                if (!context.api.allows("steam.read")) {
                    stopUnobservableForceSync();
                    return;
                }
                resetPagination();
                await load();
                startForceSyncPolling();
            });
            updateForceSyncControl();
        }
        const canForceSync = context.api.allows("steam.refresh");
        if (canForceSync) {
            forceSyncControl = Amaterasu.button("Force catalog sync", requestForceSync);
        }
        updateForceSyncControl();
        scan.disabled = !context.api.allows("steam.refresh");
        context.content.append(Amaterasu.sectionHeading("Steam library", "Discover local Steam games and optionally sync a complete scan into the catalog.", Amaterasu.button("Refresh", () => {
            resetPagination();
            return load();
        }), scan, forceSyncControl), status, area);
        async function load(refreshProviderStatus = true) {
            if (!context.api.allows("steam.read")) {
                if (context.content.isConnected) {
                    area.replaceChildren(Amaterasu.empty("Steam status unavailable", "This session can request a Steam force sync but needs steam.read to inspect discoveries or track its progress."));
                }
                return;
            }
            try {
                const providerRequest = refreshProviderStatus
                    ? context.api.post("steam/status")
                    : Promise.resolve(providerStatus);
                const [reply, provider] = await Promise.all([
                    context.api.post("steam/list", {
                        ...(revision != null ? { revision } : {}),
                        ...(after != null ? { after } : {}),
                        limit: 16,
                    }),
                    providerRequest,
                ]);
                if (!context.content.isConnected || !provider)
                    return;
                revision = reply.revision;
                providerStatus = provider;
                scan.disabled =
                    !context.api.allows("steam.refresh") || provider.enabled === false;
                if (forceSyncPending)
                    finishForceSync(provider);
                updateForceSyncControl();
                const rows = reply.games.map((game) => [
                    Amaterasu.el("div", "", Amaterasu.el("strong", "", game.name), Amaterasu.el("small", "path block", game.install_directory ?? "")),
                    String(game.app_id),
                    Amaterasu.badge(game.installed ? "Installed" : "Discovered"),
                ]);
                area.replaceChildren(Amaterasu.el("div", "section-meta", Amaterasu.badge(provider.enabled === false
                    ? "Discovery disabled"
                    : provider.scanning
                        ? "Scan in progress"
                        : provider.complete
                            ? "Scan complete"
                            : "Scan incomplete"), Amaterasu.el("span", "muted", `${provider.game_count} discoveries · revision ${reply.revision}`)), reply.games.length
                    ? Amaterasu.table(["Application", "Steam app ID", "Status"], rows)
                    : Amaterasu.empty("No Steam discoveries", provider.enabled === false
                        ? "Enable Steam discovery in Settings to scan local libraries."
                        : "Configure Steam library roots on the host, then scan. Discovery does not import or launch games."), ...(provider.warnings ?? []).map((warning) => Amaterasu.el("p", "notice", warning)), Amaterasu.pager(history.length > 1
                    ? () => {
                        history.pop();
                        after = history[history.length - 1];
                        void load();
                    }
                    : null, reply.next_after != null
                    ? () => {
                        after = reply.next_after;
                        history.push(after);
                        void load();
                    }
                    : null, `Page ${history.length}`));
            }
            catch (error) {
                if (context.content.isConnected)
                    area.replaceChildren(Amaterasu.errorPanel(error, () => {
                        revision = undefined;
                        after = undefined;
                        history.splice(0, history.length, undefined);
                        return load();
                    }));
            }
        }
        void load();
    }
    Amaterasu.renderSteam = renderSteam;
    function parseDiagnosticQueryReply(value, after) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Diagnostics query reply is invalid.");
        const reply = value;
        const nonnegative = (number) => typeof number === "number" && Number.isSafeInteger(number) && number >= 0;
        const validName = (name) => typeof name === "string" && /^[A-Za-z0-9._\-/]{1,64}$/.test(name);
        if (reply.ok !== true || !Array.isArray(reply.records) || reply.records.length > 4 ||
            typeof reply.has_more !== "boolean" || typeof reply.resync_required !== "boolean" ||
            !nonnegative(reply.next_cursor) || !nonnegative(reply.latest_sequence) ||
            !nonnegative(reply.dropped_total) ||
            (reply.oldest_sequence !== null && !nonnegative(reply.oldest_sequence)) ||
            reply.next_cursor < after || reply.next_cursor > reply.latest_sequence)
            throw new Error("Diagnostics query reply is invalid or exceeds exact browser integer range.");
        let sequence = after;
        for (const entry of reply.records) {
            if (!entry || typeof entry !== "object" || Array.isArray(entry))
                throw new Error("Diagnostics record is invalid.");
            const record = entry;
            if (!nonnegative(record.sequence) || record.sequence <= sequence ||
                !["trace", "debug", "info", "warn", "error"].includes(record.level) ||
                !validName(record.target) || typeof record.message !== "string" ||
                new TextEncoder().encode(record.message).length > 512 ||
                !record.fields || typeof record.fields !== "object" || Array.isArray(record.fields) ||
                Object.keys(record.fields).length > 8 ||
                Object.entries(record.fields).some(([name, field]) => !validName(name) ||
                    typeof field !== "string" || new TextEncoder().encode(field).length > 256))
                throw new Error("Diagnostics record is invalid.");
            sequence = record.sequence;
        }
        if (reply.next_cursor !== sequence || reply.has_more !== (sequence < reply.latest_sequence))
            throw new Error("Diagnostics query cursor is inconsistent.");
        return reply;
    }
    Amaterasu.parseDiagnosticQueryReply = parseDiagnosticQueryReply;
    function parseDiagnosticCountersReply(value) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Diagnostics counters reply is invalid.");
        const reply = value;
        if (reply.ok !== true || !reply.counters || typeof reply.counters !== "object" ||
            Array.isArray(reply.counters))
            throw new Error("Diagnostics counters reply is invalid.");
        const counters = reply.counters;
        if (Object.entries(counters).some(([name, count]) => !/^[A-Za-z0-9._\-/]{1,64}$/.test(name) ||
            typeof count !== "number" || !Number.isSafeInteger(count) || count < 0))
            throw new Error("Diagnostics counter value is invalid or exceeds exact browser integer range.");
        return counters;
    }
    Amaterasu.parseDiagnosticCountersReply = parseDiagnosticCountersReply;
    function renderDiagnostics(context) {
        const area = Amaterasu.el("div");
        let after = 0;
        const history = [0];
        let loadRequest = 0;
        context.content.append(Amaterasu.sectionHeading("Diagnostics", "Records and counters for this management console.", Amaterasu.button("Refresh", () => {
            after = 0;
            history.splice(0, history.length, 0);
            return load();
        })), area);
        async function load() {
            const request = ++loadRequest;
            const cursor = after;
            try {
                const [queryValue, counterValue] = await Promise.all([
                    context.api.post("diagnostics/query", {
                        after: cursor,
                        limit: 4,
                    }),
                    context.api.post("diagnostics/counters"),
                ]);
                if (!context.content.isConnected || request !== loadRequest)
                    return;
                const reply = parseDiagnosticQueryReply(queryValue, cursor);
                const counters = parseDiagnosticCountersReply(counterValue);
                if (reply.resync_required)
                    history.splice(0, history.length, cursor);
                const records = Amaterasu.el("div", "records", ...reply.records.map((record) => Amaterasu.el("article", "record", Amaterasu.el("div", "section-meta", Amaterasu.badge(record.level, record.level === "error" ? "bad" : ""), Amaterasu.el("small", "muted", `#${record.sequence} · ${record.target}`)), Amaterasu.el("p", "", record.message), ...Object.entries(record.fields).map(([key, value]) => Amaterasu.el("p", "record-field", Amaterasu.el("strong", "", `${key}: `), value)))));
                area.replaceChildren(reply.resync_required
                    ? Amaterasu.el("p", "notice", "Older records were removed from the bounded journal. Showing the records still available.")
                    : nullNode(), Amaterasu.panel("Recent records", reply.records.length
                    ? records
                    : Amaterasu.empty("No diagnostic records", "Records appear when a service explicitly submits them. Automatic host log collection is not enabled.")), Amaterasu.pager(history.length > 1
                    ? () => {
                        history.pop();
                        after = history[history.length - 1] ?? 0;
                        void load();
                    }
                    : null, reply.has_more
                    ? () => {
                        after = reply.next_cursor;
                        history.push(after);
                        void load();
                    }
                    : null, `Page ${history.length}`), Amaterasu.panel("Counters", Object.keys(counters).length
                    ? Amaterasu.table(["Counter", "Value"], Object.entries(counters).map(([name, value]) => [
                        name,
                        String(value),
                    ]))
                    : Amaterasu.el("p", "muted", "No counters have been recorded.")));
            }
            catch (error) {
                if (context.content.isConnected && request === loadRequest)
                    area.replaceChildren(Amaterasu.errorPanel(error, load));
            }
        }
        void load();
    }
    Amaterasu.renderDiagnostics = renderDiagnostics;
    function nullNode() {
        return document.createTextNode("");
    }
})(Amaterasu || (Amaterasu = {}));
var Amaterasu;
(function (Amaterasu) {
    function parsePendingPairings(value) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Pending pairing response is invalid.");
        const reply = value;
        if (reply.ok !== true || !Array.isArray(reply.pending))
            throw new Error("Pending pairing response is invalid.");
        const pending = reply.pending.map((item) => {
            if (!item || typeof item !== "object" || Array.isArray(item))
                throw new Error("Pending pairing entry is invalid.");
            const entry = item;
            if (typeof entry.pairing_id !== "string" ||
                !/^[a-f0-9]{32}$/.test(entry.pairing_id) ||
                typeof entry.client_id !== "string" ||
                entry.client_id.length < 1 ||
                entry.client_id.length > 64 ||
                /[\u0000-\u001f\u007f-\u009f]/.test(entry.client_id) ||
                typeof entry.name !== "string" ||
                entry.name.length < 1 ||
                entry.name.length > 128 ||
                /[\u0000-\u001f\u007f-\u009f]/.test(entry.name) ||
                !Number.isSafeInteger(entry.expires_at) ||
                entry.expires_at < 0 ||
                typeof entry.awaiting_pin !== "boolean")
                throw new Error("Pending pairing entry is invalid.");
            return {
                pairing_id: entry.pairing_id,
                client_id: entry.client_id,
                name: entry.name,
                expires_at: entry.expires_at,
                awaiting_pin: entry.awaiting_pin,
            };
        });
        return { ok: true, pending };
    }
    Amaterasu.parsePendingPairings = parsePendingPairings;
    function parseHostStatus(value) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Host status response is invalid.");
        const reply = value;
        if (reply.ok !== true ||
            typeof reply.hostname !== "string" ||
            !Number.isSafeInteger(reply.revision) ||
            !nullablePort(reply.active_http_port) ||
            !nullablePort(reply.active_https_port) ||
            !nullablePort(reply.active_rtsp_port) ||
            !nullableText(reply.listener_error) ||
            !nullableText(reply.rtsp_listener_error))
            throw new Error("Host status response is invalid.");
        return reply;
    }
    function nullablePort(value) {
        return value === null ||
            (Number.isSafeInteger(value) && value >= 1 && value <= 65535);
    }
    function nullableText(value) {
        return value === null || typeof value === "string";
    }
    function parseSessionStatus(value) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Session status response is invalid.");
        const reply = value;
        if (reply.ok !== true ||
            !Array.isArray(reply.sessions) ||
            !(reply.next_offset === null ||
                (Number.isSafeInteger(reply.next_offset) && reply.next_offset >= 0)))
            throw new Error("Session status response is invalid.");
        for (const item of reply.sessions) {
            if (!item || typeof item !== "object" || Array.isArray(item))
                throw new Error("Session status entry is invalid.");
            const session = item;
            if (!Number.isSafeInteger(session.id) ||
                session.id < 0 ||
                typeof session.key !== "string" ||
                typeof session.state !== "string" ||
                !["preparing", "active", "stopping", "stopped"].includes(session.state) ||
                !(session.outcome === null || session.outcome === undefined ||
                    ["completed", "cancelled", "failed"].includes(String(session.outcome))) ||
                !(session.lease_count === undefined ||
                    (Number.isSafeInteger(session.lease_count) && session.lease_count >= 0)))
                throw new Error("Session status entry is invalid.");
        }
        return reply;
    }
    function parseRuntimeStatus(value) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Streaming status response is invalid.");
        const reply = value;
        if (!reply.status || typeof reply.status !== "object" || Array.isArray(reply.status))
            throw new Error("Streaming status response is invalid.");
        const status = reply.status;
        if (reply.ok !== true ||
            typeof status.available !== "boolean" ||
            typeof status.configuration_ready !== "boolean" ||
            !Number.isSafeInteger(status.active_streams) ||
            status.active_streams < 0 ||
            !Number.isSafeInteger(status.failed_operations) ||
            status.failed_operations < 0)
            throw new Error("Streaming status response is invalid.");
        return reply;
    }
    function renderHostManagement(context) {
        const pairingArea = Amaterasu.el("div");
        const hostArea = Amaterasu.el("div");
        const sessionArea = Amaterasu.el("div");
        const runtimeArea = Amaterasu.el("div");
        let pairingRequest = 0;
        let hostRequest = 0;
        let sessionRequest = 0;
        let runtimeRequest = 0;
        context.content.append(Amaterasu.sectionHeading("Host & devices", "Review pending device access and the status reported by this host.", Amaterasu.button("Refresh all", () => {
            void loadPairings();
            void loadHost();
            void loadSessions();
            void loadRuntime();
        })), Amaterasu.panel("Pending device pairing", pairingArea), Amaterasu.panel("Host listeners", hostArea), Amaterasu.panel("Streaming status", runtimeArea), Amaterasu.panel("Management sessions", sessionArea));
        async function loadPairings(notice) {
            if (!context.content.isConnected)
                return;
            const request = ++pairingRequest;
            if (!context.api.allows("pairing.admin")) {
                pairingArea.replaceChildren(Amaterasu.el("p", "muted", "This account does not have permission to review or approve device pairing requests."));
                return;
            }
            pairingArea.replaceChildren(...(notice ? [pairingApprovalNotice(notice)] : []), Amaterasu.el("p", "muted", "Loading pending pairing requests…"));
            try {
                const reply = parsePendingPairings(await context.api.post("pairing/pending"));
                if (!context.content.isConnected || request !== pairingRequest)
                    return;
                if (reply.pending.length === 0) {
                    pairingArea.replaceChildren(...(notice ? [pairingApprovalNotice(notice)] : []), Amaterasu.empty("No pending requests", "New device requests will appear here when this host receives them."), Amaterasu.button("Refresh requests", () => { void loadPairings(); }));
                    return;
                }
                const cards = reply.pending.map((pending) => renderPendingPairing(pending));
                pairingArea.replaceChildren(...(notice ? [pairingApprovalNotice(notice)] : []), ...cards, Amaterasu.button("Refresh requests", () => { void loadPairings(); }));
            }
            catch (error) {
                if (context.content.isConnected && request === pairingRequest)
                    pairingArea.replaceChildren(Amaterasu.errorPanel(error, loadPairings));
            }
        }
        function renderPendingPairing(pending) {
            const card = Amaterasu.el("article", "panel pairing-request");
            const description = Amaterasu.el("p", "muted", pending.awaiting_pin
                ? "Enter the four-digit PIN shown on the requesting device to approve this pairing step."
                : "PIN approval was accepted. The device must complete its pairing exchange.");
            const status = Amaterasu.el("div");
            status.setAttribute("aria-live", "polite");
            const details = Amaterasu.el("div", "pairing-details", Amaterasu.el("strong", "", pending.name), Amaterasu.el("small", "muted block", `Client ${pending.client_id}`), Amaterasu.el("small", "muted block", `Request expires ${new Date(pending.expires_at * 1000).toLocaleString()}`), Amaterasu.badge(pending.awaiting_pin ? "PIN required" : "Pairing in progress", pending.awaiting_pin ? "accent" : "good"));
            card.append(details, description);
            if (pending.awaiting_pin) {
                const pin = Amaterasu.input("pairing_pin", "", "password");
                pin.autocomplete = "off";
                pin.spellcheck = false;
                pin.inputMode = "numeric";
                pin.maxLength = 4;
                pin.pattern = "[0-9]{4}";
                pin.required = true;
                const approve = Amaterasu.el("button", "button primary", "Approve PIN");
                approve.type = "submit";
                const form = Amaterasu.el("form", "pairing-approval", Amaterasu.field("Four-digit PIN", pin), status, approve);
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    if (approve.disabled)
                        return;
                    const submittedPin = pin.value;
                    pin.value = "";
                    if (!/^\d{4}$/.test(submittedPin)) {
                        status.replaceChildren(Amaterasu.errorPanel(new Error("Enter the four-digit PIN shown on the device.")));
                        return;
                    }
                    void Amaterasu.runAction(approve, status, async () => {
                        try {
                            await context.api.post("pairing/approve_pin", {
                                pairing_id: pending.pairing_id,
                                pin: submittedPin,
                            });
                        }
                        catch {
                            if (!context.content.isConnected)
                                return;
                            const notice = "The approval response was not confirmed. The PIN was cleared and current pairing status was refreshed; review it before trying again.";
                            context.notify("Approval was not confirmed. Current pairing status is being refreshed; the PIN was cleared.");
                            await loadPairings(notice);
                            return;
                        }
                        if (!context.content.isConnected)
                            return;
                        context.notify("PIN approval accepted. The device must complete its pairing exchange.");
                        await loadPairings();
                    });
                });
                card.append(form);
            }
            else {
                card.append(status);
            }
            return card;
        }
        async function loadHost() {
            const request = ++hostRequest;
            if (!context.api.allows("configuration.read")) {
                hostArea.replaceChildren(Amaterasu.el("p", "muted", "Host settings status requires configuration.read permission."));
                return;
            }
            hostArea.replaceChildren(Amaterasu.el("p", "muted", "Loading host listener status…"));
            try {
                const host = parseHostStatus(await context.api.post("nvhttp/settings"));
                if (!context.content.isConnected || request !== hostRequest)
                    return;
                const listener = (label, port, error) => [
                    Amaterasu.el("strong", "", label),
                    port === null
                        ? Amaterasu.badge(error ? "Error reported" : "Not confirmed")
                        : Amaterasu.badge(`Active · port ${port}`, "good"),
                ];
                const rows = [
                    listener("HTTP", host.active_http_port, host.listener_error),
                    listener("HTTPS", host.active_https_port, host.listener_error),
                    listener("RTSP", host.active_rtsp_port, host.rtsp_listener_error),
                ];
                hostArea.replaceChildren(Amaterasu.el("p", "muted", `Host ${host.hostname} · settings revision ${host.revision}`), Amaterasu.table(["Listener", "Reported status"], rows), Amaterasu.button("Refresh host status", () => { void loadHost(); }));
            }
            catch (error) {
                if (context.content.isConnected && request === hostRequest)
                    hostArea.replaceChildren(Amaterasu.errorPanel(error, loadHost));
            }
        }
        async function loadSessions() {
            const request = ++sessionRequest;
            if (!context.api.allows("session.read")) {
                sessionArea.replaceChildren(Amaterasu.el("p", "muted", "Management session status requires session.read permission."));
                return;
            }
            sessionArea.replaceChildren(Amaterasu.el("p", "muted", "Loading management sessions…"));
            try {
                const reply = parseSessionStatus(await context.api.post("session/list", { limit: 16 }));
                if (!context.content.isConnected || request !== sessionRequest)
                    return;
                const rows = reply.sessions.map((session) => [
                    Amaterasu.el("strong", "", session.key),
                    Amaterasu.badge(session.state, session.state === "active" ? "good" : ""),
                    String(session.lease_count ?? Object.keys(session.leases ?? {}).length),
                    session.outcome ?? "—",
                ]);
                sessionArea.replaceChildren(rows.length
                    ? Amaterasu.table(["Session", "State", "Leases", "Outcome"], rows)
                    : Amaterasu.empty("No management sessions", "Sessions created through the management API will appear here."), reply.next_offset === null
                    ? Amaterasu.el("p", "muted", `Showing ${reply.sessions.length} session${reply.sessions.length === 1 ? "" : "s"}.`)
                    : Amaterasu.el("p", "muted", `Showing the first ${reply.sessions.length} sessions; more are available.`), Amaterasu.button("Refresh sessions", () => { void loadSessions(); }));
            }
            catch (error) {
                if (context.content.isConnected && request === sessionRequest)
                    sessionArea.replaceChildren(Amaterasu.errorPanel(error, loadSessions));
            }
        }
        async function loadRuntime() {
            const request = ++runtimeRequest;
            if (!context.api.allows("session.read")) {
                runtimeArea.replaceChildren(Amaterasu.el("p", "muted", "Streaming status requires session.read permission."));
                return;
            }
            runtimeArea.replaceChildren(Amaterasu.el("p", "muted", "Loading streaming status…"));
            try {
                const reply = parseRuntimeStatus(await context.api.post("history/history_status"));
                if (!context.content.isConnected || request !== runtimeRequest)
                    return;
                const state = reply.status;
                runtimeArea.replaceChildren(Amaterasu.badge(state.available && state.configuration_ready ? "History service available" : "History service unavailable", state.available && state.configuration_ready ? "good" : ""), Amaterasu.el("p", "muted", `${state.active_streams} active stream${state.active_streams === 1 ? "" : "s"} reported · ${state.failed_operations} failed storage operation${state.failed_operations === 1 ? "" : "s"} since service start.`), Amaterasu.button("Refresh streaming status", () => { void loadRuntime(); }));
            }
            catch (error) {
                if (context.content.isConnected && request === runtimeRequest)
                    runtimeArea.replaceChildren(Amaterasu.errorPanel(error, loadRuntime));
            }
        }
        void loadPairings();
        void loadHost();
        void loadSessions();
        void loadRuntime();
    }
    Amaterasu.renderHostManagement = renderHostManagement;
    function pairingApprovalNotice(message) {
        const notice = Amaterasu.el("div", "notice", Amaterasu.el("strong", "", "Approval not confirmed"), Amaterasu.el("p", "", message));
        notice.setAttribute("role", "status");
        notice.setAttribute("aria-live", "polite");
        return notice;
    }
})(Amaterasu || (Amaterasu = {}));
var Amaterasu;
(function (Amaterasu) {
    const pageNames = {
        overview: "Overview",
        host: "Host & devices",
        catalog: "Library",
        settings: "Settings",
        updates: "Modules & UI",
        sessions: "Sessions",
        applications: "Applications",
        steam: "Steam",
        diagnostics: "Diagnostics",
    };
    const pageIcons = {
        updates: "+",
        host: "⌂",
        overview: "▦",
        catalog: "⊞",
        settings: "≡",
        sessions: "⇄",
        applications: "□",
        steam: "◇",
        diagnostics: "⌁",
    };
    class Application {
        api;
        root;
        signedIn = false;
        content = Amaterasu.el("main");
        navigation = new Map();
        notification = Amaterasu.el("div");
        notificationTimer = 0;
        restorationGeneration = 0;
        constructor() {
            this.root = document.getElementById("app");
            document
                .querySelector(".skip-link")
                ?.addEventListener("click", (event) => {
                event.preventDefault();
                const main = document.getElementById("main");
                main?.focus();
                main?.scrollIntoView();
            });
            this.api = new Amaterasu.Api(document.querySelector('meta[name="amaterasu-api-base"]')?.content ?? "");
            this.api.onAuthenticationLost = () => {
                if (this.signedIn) {
                    clearTimeout(this.notificationTimer);
                    document.querySelectorAll("dialog").forEach((dialog) => dialog.remove());
                    this.loginScreen();
                }
            };
            window.addEventListener("hashchange", () => {
                if (this.signedIn)
                    this.renderPage(this.currentPage());
            });
            window.addEventListener("pagehide", () => this.api.suspend());
            window.addEventListener("pageshow", (event) => {
                if (event.persisted) {
                    void this.restoreRememberedSession();
                }
            });
            void this.restoreRememberedSession();
        }
        async restoreRememberedSession() {
            const generation = ++this.restorationGeneration;
            this.sessionRestoringScreen();
            let restored = false;
            try {
                restored = await this.api.restoreSession();
            }
            catch {
            }
            if (generation !== this.restorationGeneration)
                return;
            if (restored)
                this.shell();
            else
                this.loginScreen();
        }
        sessionRestoringScreen() {
            this.signedIn = false;
            const main = Amaterasu.el("main", "login-layout");
            main.id = "main";
            main.tabIndex = -1;
            main.setAttribute("aria-busy", "true");
            const intro = Amaterasu.el("section", "login-intro", this.brand(), Amaterasu.el("div", "", Amaterasu.badge("LOCAL HOST MANAGEMENT", "accent"), Amaterasu.el("h1", "", "Your host.\nWithin reach."), Amaterasu.el("p", "", "Checking for a remembered session.")));
            const status = Amaterasu.el("section", "login-card", Amaterasu.el("p", "eyebrow", "WELCOME BACK"), Amaterasu.el("h2", "", "Restoring your session"), Amaterasu.el("p", "muted", "Please wait while we check this browser."));
            status.setAttribute("role", "status");
            status.setAttribute("aria-live", "polite");
            status.setAttribute("aria-atomic", "true");
            main.append(intro, Amaterasu.el("div", "login-side", status));
            this.root.replaceChildren(main);
            document.title = "Restoring session · Amaterasu";
        }
        currentPage() {
            const value = location.hash.slice(1);
            return Object.hasOwn(pageNames, value) ? value : "overview";
        }
        navigate(page) {
            if (location.hash === `#${page}`)
                this.renderPage(page);
            else
                location.hash = page;
        }
        loginScreen() {
            this.signedIn = false;
            const main = Amaterasu.el("main", "login-layout");
            main.id = "main";
            main.tabIndex = -1;
            const brand = this.brand();
            const intro = Amaterasu.el("section", "login-intro", brand, Amaterasu.el("div", "", Amaterasu.badge("LOCAL HOST MANAGEMENT", "accent"), Amaterasu.el("h1", "", "Your host.\nWithin reach."), Amaterasu.el("p", "", "A considered place to organize applications, manage sessions, and keep an eye on your host.")), Amaterasu.el("p", "login-footnote", "Independent components. One clear view."));
            const card = Amaterasu.el("section", "login-card", Amaterasu.el("p", "eyebrow", "WELCOME BACK"), Amaterasu.el("h2", "", "Connect to your host"), Amaterasu.el("p", "muted", "Sign in with the administrator credentials configured on this host."));
            const username = Amaterasu.input("username");
            username.autocomplete = "username";
            username.required = true;
            const password = Amaterasu.input("password", "", "password");
            password.autocomplete = "current-password";
            password.required = true;
            const rememberMe = Amaterasu.input("remember_me", "", "checkbox");
            const rememberField = Amaterasu.field("Remember this browser", rememberMe, this.api.usesSecureCookies
                ? "Keep this signed-in browser session across restarts. Session cookies are protected from scripts."
                : "Store raw tokens in one localStorage record across reloads and restarts. Same-origin scripts can read it. Remember me requires Web Locks in a secure browser context; sign in without it when unavailable.");
            const token = Amaterasu.input("token", "", "password");
            token.autocomplete = "off";
            token.spellcheck = false;
            const credentials = Amaterasu.el("div", "login-fields", Amaterasu.field("Username", username), Amaterasu.field("Password", password));
            const tokenField = Amaterasu.field("API token", token, "Token scope is enforced by the host on every request.");
            tokenField.hidden = true;
            const status = Amaterasu.el("div");
            status.setAttribute("aria-live", "polite");
            const submit = Amaterasu.el("button", "button primary full-width", "Sign in →");
            submit.type = "submit";
            let tokenMode = false;
            const toggle = Amaterasu.button("Use an API token instead", () => {
                tokenMode = !tokenMode;
                credentials.hidden = tokenMode;
                tokenField.hidden = !tokenMode;
                username.required = !tokenMode;
                password.required = !tokenMode;
                token.required = tokenMode;
                password.value = "";
                rememberMe.checked = false;
                token.value = "";
                rememberField.hidden = tokenMode;
                status.replaceChildren();
                toggle.textContent = tokenMode
                    ? "Use username and password"
                    : "Use an API token instead";
                (tokenMode ? token : username).focus();
            }, "link-button");
            const form = Amaterasu.el("form", "", credentials, rememberField, tokenField, status, submit, toggle);
            form.addEventListener("submit", (event) => {
                event.preventDefault();
                if (!form.reportValidity())
                    return;
                void Amaterasu.runAction(submit, status, async () => {
                    if (tokenMode)
                        await this.api.useToken(token.value.trim());
                    else
                        await this.api.login(username.value.trim(), password.value, rememberMe.checked);
                    password.value = "";
                    token.value = "";
                    this.shell();
                });
            });
            const setup = Amaterasu.el("details", "setup-note", Amaterasu.el("summary", "", "First time setting up this host?"), Amaterasu.el("p", "", "An administrator must create the initial credentials through the trusted local console. Password setup is not available in this browser."), Amaterasu.el("p", "muted", "The management listener must be enabled and this browser origin allowed. Contact the person who configured the host."));
            card.append(form, setup, Amaterasu.el("div", "login-security", Amaterasu.el("span", "status-dot"), Amaterasu.el("span", "", this.api.usesSecureCookies
                ? "Your browser sends protected session cookies only to this HTTPS host."
                : "Same-origin scripts can read remembered tokens; localStorage is not HttpOnly protection or a native-code sandbox.")));
            main.append(intro, Amaterasu.el("div", "login-side", card));
            this.root.replaceChildren(main);
            username.focus();
            document.title = "Sign in · Amaterasu";
        }
        brand() {
            return Amaterasu.el("div", "brand", Amaterasu.el("span", "brand-symbol", Amaterasu.el("span")), Amaterasu.el("span", "", "amaterasu"));
        }
        shell() {
            this.signedIn = true;
            this.navigation.clear();
            const shell = Amaterasu.el("div", "shell");
            const sidebar = Amaterasu.el("aside", "sidebar");
            const nav = Amaterasu.el("nav", "navigation");
            nav.setAttribute("aria-label", "Main navigation");
            for (const page of Object.keys(pageNames)) {
                const icon = Amaterasu.el("span", "nav-icon", pageIcons[page]);
                icon.setAttribute("aria-hidden", "true");
                const item = Amaterasu.button(pageNames[page], () => {
                    shell.classList.remove("menu-open");
                    menu.setAttribute("aria-expanded", "false");
                    this.navigate(page);
                }, "nav-item");
                item.prepend(icon);
                nav.append(item);
                this.navigation.set(page, item);
            }
            sidebar.append(this.brand(), Amaterasu.el("p", "nav-heading", "WORKSPACE"), nav, Amaterasu.el("div", "sidebar-bottom", Amaterasu.badge("Development preview"), Amaterasu.el("p", "muted", "Local management console"), Amaterasu.el("small", "muted", "Streaming features are in progress.")));
            const menu = Amaterasu.button("☰", () => {
                const open = shell.classList.toggle("menu-open");
                menu.setAttribute("aria-expanded", String(open));
            }, "menu-toggle");
            menu.setAttribute("aria-label", "Toggle navigation");
            menu.setAttribute("aria-expanded", "false");
            const logout = Amaterasu.button("Sign out", () => {
                logout.disabled = true;
                void this.api.logout().then(() => {
                    clearTimeout(this.notificationTimer);
                    document.querySelectorAll("dialog").forEach((dialog) => dialog.remove());
                    this.loginScreen();
                }).catch((error) => {
                    const message = error instanceof Error
                        ? error.message
                        : "Could not confirm sign-out. Retry when the host is reachable.";
                    if (error instanceof Amaterasu.ApiError && error.code === "storage_unavailable" &&
                        this.api.subject) {
                        logout.disabled = false;
                        this.notify(message);
                        return;
                    }
                    clearTimeout(this.notificationTimer);
                    document.querySelectorAll("dialog").forEach((dialog) => dialog.remove());
                    this.loginScreen();
                    const warning = Amaterasu.el("p", "error", message);
                    warning.setAttribute("role", "alert");
                    this.root.querySelector(".login-card")?.prepend(warning);
                });
            }, "quiet");
            const top = Amaterasu.el("header", "topbar", Amaterasu.el("div", "actions", menu, Amaterasu.el("span", "topbar-label", "Host workspace")), Amaterasu.el("div", "account", Amaterasu.el("span", "account-avatar", this.api.subject.slice(0, 1).toUpperCase()), Amaterasu.el("span", "account-name", this.api.subject), Amaterasu.mountLocaleSelector(this.api), logout));
            this.notification = Amaterasu.el("div", "toast");
            this.notification.setAttribute("role", "status");
            this.notification.hidden = true;
            this.content = Amaterasu.el("main", "content");
            this.content.id = "main";
            this.content.tabIndex = -1;
            shell.append(sidebar, Amaterasu.el("div", "workspace", top, this.content), this.notification);
            this.root.replaceChildren(shell);
            this.renderPage(this.currentPage());
        }
        renderPage(page) {
            const fresh = Amaterasu.el("main", "content");
            fresh.id = "main";
            fresh.tabIndex = -1;
            this.content.replaceWith(fresh);
            this.content = fresh;
            for (const [name, item] of this.navigation) {
                item.classList.toggle("selected", name === page);
                if (name === page)
                    item.setAttribute("aria-current", "page");
                else
                    item.removeAttribute("aria-current");
            }
            const context = {
                api: this.api,
                content: fresh,
                notify: (message) => this.notify(message),
                navigate: (page) => this.navigate(page),
            };
            const renderers = {
                overview: Amaterasu.renderOverview,
                host: Amaterasu.renderHostManagement,
                catalog: Amaterasu.renderCatalog,
                settings: Amaterasu.renderSettings,
                updates: Amaterasu.renderUpdates,
                sessions: Amaterasu.renderSessions,
                applications: Amaterasu.renderApplications,
                steam: Amaterasu.renderSteam,
                diagnostics: Amaterasu.renderDiagnostics,
            };
            renderers[page](context);
            document.title = `${pageNames[page]} · Amaterasu`;
            fresh.focus({ preventScroll: true });
            window.scrollTo(0, 0);
        }
        notify(message) {
            this.notification.textContent = message;
            this.notification.hidden = false;
            clearTimeout(this.notificationTimer);
            this.notificationTimer = window.setTimeout(() => {
                this.notification.hidden = true;
            }, 5000);
        }
    }
    new Application();
})(Amaterasu || (Amaterasu = {}));
