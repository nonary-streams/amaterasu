"use strict";
var Amaterasu;
(function (Amaterasu) {
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
    function providerConfigOverrides(value, streamAudioValue = "", audioSinkMode = "inherit", audioSinkId = "", audioSinkCaptureOnlyValue = "") {
        return {
            fec_percentage: parseFecOverride(value),
            stream_audio: parseStreamAudioOverride(streamAudioValue),
            audio_sink: parseAudioSinkOverride(audioSinkMode, audioSinkId),
            audio_sink_capture_only: parseAudioSinkCaptureOnlyOverride(audioSinkCaptureOnlyValue),
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
    function parsePreparation(value) {
        const parsed = JSON.parse(value || "[]");
        if (!Array.isArray(parsed) ||
            parsed.some((step) => step === null ||
                Array.isArray(step) ||
                typeof step !== "object" ||
                !step.run ||
                Array.isArray(step.run) ||
                typeof step.run !== "object")) {
            throw new Error("Existing preparation commands are malformed; reload before saving.");
        }
        return parsed;
    }
    function entryFromForm(data) {
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
        const steamId = kind === "playnite_fullscreen" ? null : parseSteamId(String(data.get("steam_id") ?? ""));
        const steamInstallDir = kind === "playnite_fullscreen" ? null : parseSteamInstallDir(String(data.get("steam_install_dir") ?? ""));
        const configOverrides = fecPercentage === null && streamAudio === null && audioSink === null && audioSinkCaptureOnly === null
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
        tokenGeneration = 0;
        refreshInFlight = null;
        requests = new Set();
        base;
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
        }
        async login(username, password, rememberMe = false) {
            const generation = this.authGeneration;
            const reply = await this.post("auth/login", { username, password, remember_me: rememberMe }, false);
            this.installSession(reply, generation, true);
        }
        useToken(token) {
            if (!/^ama_[a-f0-9]{64}$/.test(token))
                throw new Error("Enter an Amaterasu token beginning with ama_ followed by 64 hexadecimal characters.");
            this.clear();
            this.bearer = token;
            this.subject = "API token";
            this.scopes = null;
            this.tokenGeneration += 1;
        }
        async logout() {
            const payload = this.refreshToken
                ? { refresh_token: this.refreshToken }
                : this.bearer
                    ? { bearer: this.bearer }
                    : null;
            this.clear();
            if (payload) {
                try {
                    await this.post("auth/logout", payload, false);
                }
                catch {
                }
            }
        }
        clear() {
            this.authGeneration += 1;
            this.tokenGeneration += 1;
            this.bearer = "";
            this.refreshToken = "";
            this.accessExpiresAt = 0;
            this.subject = "";
            this.scopes = null;
            this.refreshInFlight = null;
            for (const request of this.requests)
                request.abort();
            this.requests.clear();
        }
        allows(scope) {
            return this.scopes === null || this.scopes.includes(scope);
        }
        async post(route, payload = {}, protectedRoute = true) {
            if (protectedRoute && !this.bearer)
                throw new ApiError("Sign in to continue.", "unauthorized", 401);
            if (protectedRoute &&
                this.refreshToken &&
                this.accessExpiresAt - Date.now() / 1000 <= 60) {
                await this.refreshOrClear();
            }
            const bearerUsed = this.bearer;
            const authGeneration = this.authGeneration;
            const tokenGeneration = this.tokenGeneration;
            try {
                return await this.send(route, payload, protectedRoute, bearerUsed);
            }
            catch (error) {
                if (!protectedRoute ||
                    !(error instanceof ApiError) ||
                    error.code !== "unauthorized" ||
                    !this.refreshToken ||
                    authGeneration !== this.authGeneration) {
                    throw error;
                }
                if (tokenGeneration === this.tokenGeneration)
                    await this.refreshOrClear();
                return this.send(route, payload, true, this.bearer);
            }
        }
        async refreshOrClear() {
            try {
                await this.refreshSession();
            }
            catch (error) {
                if (this.refreshToken) {
                    this.clear();
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
            const token = this.refreshToken;
            const pending = (async () => {
                const reply = await this.send("auth/refresh", { refresh_token: token }, false, "");
                if (generation !== this.authGeneration || token !== this.refreshToken)
                    throw new Error("The sign-in session was cleared.");
                this.installSession(reply, generation, false);
            })();
            this.refreshInFlight = pending;
            try {
                await pending;
            }
            finally {
                if (this.refreshInFlight === pending)
                    this.refreshInFlight = null;
            }
        }
        installSession(reply, generation, newSession) {
            if (generation !== this.authGeneration)
                throw new Error("The sign-in attempt was cancelled.");
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
            this.bearer = reply.bearer;
            this.refreshToken = reply.refresh_token;
            this.accessExpiresAt = reply.expires_at;
            this.subject = reply.subject;
            this.scopes = reply.scopes;
            this.tokenGeneration += 1;
            if (newSession)
                this.authGeneration += 1;
        }
        async send(route, payload, protectedRoute, bearer) {
            const controller = new AbortController();
            const timer = window.setTimeout(() => controller.abort(), 12_000);
            this.requests.add(controller);
            try {
                const headers = {
                    "Content-Type": "application/json",
                };
                if (protectedRoute)
                    headers.Authorization = `Bearer ${bearer}`;
                const response = await fetch(`${this.base}/v1/${route}`, {
                    method: "POST",
                    headers,
                    body: JSON.stringify(payload),
                    signal: controller.signal,
                    credentials: "omit",
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
        const add = Amaterasu.button("+ Add application", () => showEditor(), "primary");
        add.disabled = !api.allows("catalog.write");
        content.append(Amaterasu.sectionHeading("Your library", "A durable catalog of the applications on your host.", add), searchForm, status, editor, results);
        async function load() {
            const current = ++generation;
            results.setAttribute("aria-busy", "true");
            try {
                const reply = await api.post(query ? "catalog/search" : "catalog/list", { ...(query ? { query } : {}), cursor: cursors[page], limit: 12 });
                if (!content.isConnected || current !== generation)
                    return;
                revision = reply.revision;
                const grid = Amaterasu.el("div", "catalog-grid");
                for (const row of reply.entries)
                    grid.append(card(row));
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
        function card(row) {
            const providerLaunch = row.entry.launch.kind === "playnite";
            const edit = Amaterasu.button("Edit", () => showEditor(row));
            edit.disabled = !api.allows("catalog.write") || providerLaunch;
            if (providerLaunch)
                edit.title = "Playnite launch targets are managed by the provider.";
            const actions = [edit];
            if (providerLaunch) {
                const configure = Amaterasu.button("Streaming settings", () => showProviderOverrideEditor(row), "secondary");
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
            if (row.entry.artwork?.provider === "steam" && api.allows("steam.read")) {
                void readSteamArtwork(api, row.entry.artwork).then((blob) => {
                    if (!content.isConnected || !art.isConnected)
                        return;
                    const url = URL.createObjectURL(blob);
                    const image = Amaterasu.el("img");
                    image.alt = "";
                    image.addEventListener("load", () => URL.revokeObjectURL(url), { once: true });
                    image.addEventListener("error", () => URL.revokeObjectURL(url), { once: true });
                    image.src = url;
                    art.prepend(image);
                }).catch(() => {
                });
            }
            return Amaterasu.el("article", "app-card", art, Amaterasu.el("div", "app-details", Amaterasu.el("h2", "", row.entry.name), Amaterasu.el("p", "muted description", row.entry.description || "No description added."), Amaterasu.el("div", "tags", ...(row.entry.tags ?? []).map((tag) => Amaterasu.badge(tag))), Amaterasu.el("p", "path", Amaterasu.launchPath(row.entry)), Amaterasu.el("div", "card-footer", ...actions)));
        }
        function showProviderOverrideEditor(row) {
            let editRevision = revision;
            const currentFec = row.entry.config_overrides?.fec_percentage;
            const fec = Amaterasu.input("fec_percentage", currentFec === undefined ? "" : String(currentFec), "number");
            fec.min = "1";
            fec.max = "255";
            fec.step = "1";
            fec.placeholder = "Inherit";
            fec.setAttribute("aria-label", `FEC percentage override for ${row.entry.name}`);
            const streamAudio = audioOverrideSelect("stream_audio", row.entry.config_overrides?.stream_audio);
            const audioSink = audioSinkOverrideControls(row.entry.config_overrides?.audio_sink);
            const captureOnly = audioOverrideSelect("audio_sink_capture_only", row.entry.config_overrides?.audio_sink_capture_only);
            const form = Amaterasu.el("form", "editor-form", Amaterasu.field("FEC percentage", fec, "Leave blank to inherit the global setting. Supported values are 1–255."), Amaterasu.field("Audio streaming", streamAudio, "Inherit the host setting, or choose whether this game streams audio."), audioSink.controls, Amaterasu.field("Capture audio without changing default outputs", captureOnly, "Inherit the host setting, enable capture only, or allow Windows default-output routing."));
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
                    const config_overrides = Amaterasu.providerConfigOverrides(fec.value, streamAudio.value, audioSink.mode.value, audioSink.id.value, captureOnly.value);
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
        function showEditor(row) {
            const editRevision = revision;
            const form = entryForm(row?.entry);
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
                    const entry = Amaterasu.entryFromForm(new FormData(form));
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
    function entryForm(entry) {
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
        const streamAudio = audioOverrideSelect("stream_audio", entry?.config_overrides?.stream_audio);
        const audioSink = audioSinkOverrideControls(entry?.config_overrides?.audio_sink);
        const captureOnly = audioOverrideSelect("audio_sink_capture_only", entry?.config_overrides?.audio_sink_capture_only);
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
        form.append(Amaterasu.el("div", "form-grid", Amaterasu.field("Name", name), Amaterasu.field("Launch type", kind)), Amaterasu.field("Description", description), Amaterasu.field("Tags", tags, "Separate tags with commas."), pathField, Amaterasu.field("FEC percentage", fec, "Leave blank to inherit the global streaming setting; supported values are 1–255."), Amaterasu.field("Audio streaming", streamAudio, "Inherit the host setting, or choose whether this application streams audio."), audioSink.controls, Amaterasu.field("Capture audio without changing default outputs", captureOnly, "Inherit the host setting, enable capture only, or allow Windows default-output routing."), steamIdField, steamInstallDirField, fullscreen, executable);
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
    function parseHistorySettings(enabled, ttlDays, sizeLimitMb) {
        if (typeof enabled !== "boolean" ||
            !Number.isSafeInteger(ttlDays) ||
            ttlDays < 0 ||
            ttlDays > maxHistoryLimit ||
            !Number.isSafeInteger(sizeLimitMb) ||
            sizeLimitMb < 0 ||
            sizeLimitMb > maxHistoryLimit)
            throw new Error("History limits must be whole numbers from 0 to 2147483647.");
        return { enabled, ttl_days: ttlDays, db_size_limit_mb: sizeLimitMb };
    }
    Amaterasu.parseHistorySettings = parseHistorySettings;
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
    function parseInputPolicy(keyboard, mouse, highResolutionScrolling, alwaysSendScancodes, keyRightAltToKeyWin, keyRepeatDelay, keyRepeatFrequency, backButtonTimeout = -1) {
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
            backButtonTimeout > maxInputBackButtonTimeoutMs)
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
        };
    }
    Amaterasu.parseInputPolicy = parseInputPolicy;
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
    function inputPolicyStatusDescription(settings) {
        if (!settings.persisted)
            return "The saved input policy has not been confirmed yet.";
        if (!settings.applied || settings.cleanup_required)
            return "The saved policy is active for new events, but cleanup of held native input is still pending.";
        return "The saved keyboard and mouse policy is active.";
    }
    Amaterasu.inputPolicyStatusDescription = inputPolicyStatusDescription;
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
        const unique = new Set();
        for (const item of value) {
            if (typeof item !== "string" ||
                item.trim().length === 0 ||
                encoder.encode(item).length > 512 ||
                /[\u0000-\u001f\u007f-\u009f]/.test(item) ||
                unique.has(item.toLowerCase()))
                throw new Error(`${label} must contain unique, printable strings of at most 512 UTF-8 bytes.`);
            unique.add(item.toLowerCase());
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
        const authArea = Amaterasu.el("div");
        const hostArea = Amaterasu.el("div");
        const steamArea = Amaterasu.el("div");
        const lutrisArea = Amaterasu.el("div");
        const playniteArea = Amaterasu.el("div");
        const inputArea = Amaterasu.el("div");
        const historyArea = Amaterasu.el("div");
        const pairingArea = Amaterasu.el("div");
        context.content.append(Amaterasu.sectionHeading("Settings", "Edit host options and values registered for this management console.", Amaterasu.button("Refresh", () => {
            void load();
            void loadAuth();
            void loadHost();
            void loadSteam();
            void loadLutris();
            void loadPlaynite();
            void loadInput();
            void loadHistorySettings();
            void loadPairing();
        })), area, authArea, hostArea, steamArea, lutrisArea, playniteArea, inputArea, historyArea, pairingArea);
        async function loadAuth() {
            authArea.replaceChildren(Amaterasu.el("p", "muted", "Loading authentication lifetimes…"));
            try {
                const settings = await context.api.post("auth/settings");
                if (!context.content.isConnected)
                    return;
                const form = Amaterasu.el("form", "settings-form");
                const sessionTtl = Amaterasu.input("session_token_ttl_seconds", String(settings.values.session_token_ttl_seconds), "number");
                const refreshTtl = Amaterasu.input("remember_me_refresh_token_ttl_seconds", String(settings.values.remember_me_refresh_token_ttl_seconds), "number");
                for (const control of [sessionTtl, refreshTtl]) {
                    control.required = true;
                    control.max = String(maxConfiguredAuthTtlSeconds);
                }
                sessionTtl.min = "60";
                sessionTtl.step = "60";
                refreshTtl.min = "3600";
                refreshTtl.step = "3600";
                const status = Amaterasu.el("div");
                const save = Amaterasu.el("button", "button primary", "Save authentication settings");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                form.append(Amaterasu.field("Administrator session lifetime (seconds)", sessionTtl, "Access sessions default to 7200 seconds. Existing sessions keep their issued expiry; new logins and refreshes use this value."), Amaterasu.field("Remembered device lifetime (seconds)", refreshTtl, "Remember-me refresh tokens default to 604800 seconds. Non-remembered refresh tokens remain fixed at 86400 seconds."), status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${settings.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, status, async () => {
                        const values = parseAuthTokenLifetimes(sessionTtl.valueAsNumber, refreshTtl.valueAsNumber);
                        await context.api.post("auth/settings_update", {
                            ...values,
                            expected_revision: settings.revision,
                        });
                        context.notify("Authentication lifetimes saved.");
                        await loadAuth();
                    });
                });
                authArea.replaceChildren(Amaterasu.panel("Authentication session lifetimes", form));
            }
            catch (error) {
                if (context.content.isConnected)
                    authArea.replaceChildren(Amaterasu.errorPanel(error, loadAuth));
            }
        }
        async function loadSteam() {
            steamArea.replaceChildren(Amaterasu.el("p", "muted", "Loading Steam settings…"));
            try {
                const settings = await context.api.post("steam/settings");
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
                form.append(Amaterasu.field("Enable Steam library discovery", enabled, "Controls whether the provider exposes discovered Steam games and accepts scans."), Amaterasu.field("Automatic synchronization", autoSync, "Controls automatic scans of the configured Steam library roots."), Amaterasu.field("Include tools", includeTools, "Includes tools alongside games when importing the library."), Amaterasu.field("Synchronize all installed games", syncAllInstalled, "When off, import recently played games instead of every installed game."), Amaterasu.field("Remove uninstalled games during synchronization", autosyncRemoveUninstalled, "Applies when synchronizing all installed games. Recent-game mode always removes stale imported entries."), Amaterasu.field("Recent games to import", recentGames, "Maximum number of recently played games (0–10000); zero imports none in recent-only mode."), Amaterasu.field("Recent game age limit (days)", recentMaxAgeDays, "Ignore games older than this age (0–36500); zero disables the age cutoff."), Amaterasu.field("Excluded games (JSON)", exclusions, 'Use objects with "id" (canonical app ID), "name", or both. ID takes precedence; name-only entries match without case sensitivity.'), runtimeStatus, status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${settings.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, status, async () => {
                        await context.api.post("steam/settings_update", {
                            enabled: enabled.checked,
                            auto_sync: autoSync.checked,
                            exclude_games: parseSteamExclusions(exclusions.value),
                            ...parseSteamImportSettings(includeTools.checked, syncAllInstalled.checked, autosyncRemoveUninstalled.checked, recentGames.valueAsNumber, recentMaxAgeDays.valueAsNumber),
                            expected_revision: settings.revision,
                        });
                        context.notify("Steam discovery setting saved.");
                        await loadSteam();
                    });
                });
                steamArea.replaceChildren(Amaterasu.panel("Steam", form));
                void loadSteamRuntimeStatus(runtimeStatus, context);
            }
            catch (error) {
                if (context.content.isConnected)
                    steamArea.replaceChildren(Amaterasu.errorPanel(error, loadSteam));
            }
        }
        async function loadLutris() {
            lutrisArea.replaceChildren(Amaterasu.el("p", "muted", "Loading Lutris settings…"));
            try {
                const [settings, runtime] = await Promise.all([
                    loadLutrisSettings(),
                    context.api.post("lutris/status"),
                ]);
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
                const runtimeStatus = Amaterasu.el("p", runtime.last_error ? "error" : "muted", lutrisStatusDescription(runtime));
                const saveStatus = Amaterasu.el("div");
                const refreshStatus = Amaterasu.el("div");
                const save = Amaterasu.el("button", "button primary", "Save Lutris settings");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                const refresh = Amaterasu.button("Refresh now", () => Amaterasu.runAction(refresh, refreshStatus, async () => {
                    const updated = await context.api.post("lutris/refresh");
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
                        try {
                            await context.api.post("lutris/settings_update", {
                                enabled: enabled.checked,
                                auto_sync: autoSync.checked,
                                autosync_remove_uninstalled: removeUninstalled.checked,
                                include_steam: includeSteam.checked,
                                exclude_games: parseLutrisExclusions(exclusions.value),
                                expected_revision: settings.revision,
                            });
                            context.notify("Lutris settings saved.");
                            await loadLutris();
                        }
                        catch (error) {
                            if (error instanceof Amaterasu.ApiError && error.code === "revision_conflict") {
                                context.notify("Lutris settings changed elsewhere. The latest values have been reloaded.");
                                await loadLutris();
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
        async function loadLutrisSettings() {
            for (let attempt = 0;; attempt += 1) {
                try {
                    return await context.api.post("lutris/settings");
                }
                catch (error) {
                    if (!(error instanceof Amaterasu.ApiError) ||
                        !["settings_loading", "settings_busy"].includes(error.code) ||
                        attempt >= 7) {
                        throw error;
                    }
                    await new Promise((resolve) => {
                        window.setTimeout(resolve, 50 * (attempt + 1));
                    });
                }
            }
        }
        async function loadPlaynite() {
            playniteArea.replaceChildren(Amaterasu.el("p", "muted", "Loading Playnite settings…"));
            try {
                const settings = await loadPlayniteSettings();
                const runtime = await context.api.post("playnite/status");
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
                const runtimeStatus = Amaterasu.el("p", runtime.last_error ? "error" : "muted", playniteStatusDescription(runtime));
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
                            await context.api.post("playnite/settings_update", {
                                ...values,
                                expected_revision: settings.revision,
                            });
                            context.notify("Playnite settings saved.");
                            await loadPlaynite();
                        }
                        catch (error) {
                            if (error instanceof Amaterasu.ApiError && error.code === "revision_conflict") {
                                context.notify("Playnite settings changed elsewhere. The latest values have been reloaded.");
                                await loadPlaynite();
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
        async function loadPlayniteSettings() {
            for (let attempt = 0;; attempt += 1) {
                try {
                    return await context.api.post("playnite/settings");
                }
                catch (error) {
                    if (!(error instanceof Amaterasu.ApiError) ||
                        !["settings_loading", "settings_busy"].includes(error.code) ||
                        attempt >= 7) {
                        throw error;
                    }
                    await new Promise((resolve) => {
                        window.setTimeout(resolve, 50 * (attempt + 1));
                    });
                }
            }
        }
        async function loadPairing() {
            pairingArea.replaceChildren(Amaterasu.el("p", "muted", "Loading paired clients…"));
            try {
                const settings = await context.api.post("pairing/list_clients");
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
                    const save = Amaterasu.button("Save", () => Amaterasu.runAction(save, status, async () => {
                        try {
                            await context.api.post("pairing/update_client_config_overrides", {
                                certificate_id: client.certificate_id,
                                expected_revision: settings.revision,
                                config_overrides: {
                                    ...parseClientFecOverride(fec.value),
                                    stream_audio: Amaterasu.parseStreamAudioOverride(streamAudio.value),
                                    audio_sink: Amaterasu.parseAudioSinkOverride(audioSinkMode.value, audioSinkId.value),
                                    audio_sink_capture_only: Amaterasu.parseAudioSinkCaptureOnlyOverride(captureOnly.value),
                                },
                            });
                            context.notify(`Streaming override saved for ${client.name}.`);
                            await loadPairing();
                        }
                        catch (error) {
                            if (error instanceof Amaterasu.ApiError && error.code === "revision_conflict") {
                                context.notify("Paired clients changed elsewhere. The list has been refreshed; review and save again.");
                                await loadPairing();
                                return;
                            }
                            throw error;
                        }
                    }), "primary");
                    save.disabled = !context.api.allows("pairing.admin");
                    const identity = Amaterasu.el("div", "", Amaterasu.el("strong", "", client.name), Amaterasu.el("small", "muted", `${client.client_id} · certificate ${client.certificate_id.slice(0, 12)}…`));
                    return [identity, fec, streamAudio, Amaterasu.el("div", "", audioSinkMode, audioSinkId), captureOnly, Amaterasu.el("div", "", save, status)];
                });
                const tableView = Amaterasu.table(["Paired client", "FEC percentage", "Audio streaming", "Audio endpoint", "Capture only", ""], rows);
                pairingArea.replaceChildren(Amaterasu.panel("Paired client overrides", Amaterasu.el("p", "muted", "Blank inherits the application and host value. Each save uses the current pairing revision."), tableView, Amaterasu.el("div", "actions", Amaterasu.badge(`Revision ${settings.revision}`))));
            }
            catch (error) {
                if (context.content.isConnected)
                    pairingArea.replaceChildren(Amaterasu.errorPanel(error, loadPairing));
            }
        }
        async function loadHistorySettings() {
            historyArea.replaceChildren(Amaterasu.el("p", "muted", "Loading session history settings…"));
            try {
                const reply = await context.api.post("history/settings_get");
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
                const updateVisibility = () => {
                    ttlField.hidden = !enabled.checked;
                    sizeField.hidden = !enabled.checked;
                };
                enabled.addEventListener("change", updateVisibility);
                updateVisibility();
                const status = Amaterasu.el("p", "muted", reply.effective_enabled === reply.settings.enabled
                    ? "The saved history policy is active."
                    : "The enable change will apply after active streams end.");
                const save = Amaterasu.el("button", "button primary", "Save history settings");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                form.append(Amaterasu.field("Record session history", enabled, "Changes to recording during an active stream take effect after streams end."), ttlField, sizeField, status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${reply.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, status, async () => {
                        const updated = await context.api.post("history/settings_update", {
                            settings: parseHistorySettings(enabled.checked, ttl.valueAsNumber, size.valueAsNumber),
                            expected_revision: reply.revision,
                        });
                        context.notify(updated.effective_enabled === updated.settings.enabled
                            ? "Session history settings saved."
                            : "Session history settings saved; the recording change is deferred.");
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
                const settings = await context.api.post("input/settings_get");
                if (!context.content.isConnected)
                    return;
                const form = Amaterasu.el("form", "settings-form");
                const keyboard = Amaterasu.input("input_keyboard", "", "checkbox");
                keyboard.checked = settings.values.keyboard;
                const mouse = Amaterasu.input("input_mouse", "", "checkbox");
                mouse.checked = settings.values.mouse;
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
                const status = Amaterasu.el("p", settings.applied ? "muted" : "error", inputPolicyStatusDescription(settings));
                const save = Amaterasu.el("button", "button primary", "Save input policy");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                form.append(Amaterasu.field("Keyboard input", keyboard, "Disabling keyboard input suppresses key events and releases held keys immediately."), Amaterasu.field("Mouse input", mouse, "Disabling mouse input suppresses pointer, button, and scroll events and releases held buttons."), Amaterasu.field("High-resolution scrolling", highResolutionScrolling, "Preserves raw wheel units for supported streaming clients. Disable to convert scroll input to legacy 120-unit detents."), Amaterasu.field("Always send scan codes", alwaysSendScancodes, "On Windows, maps non-normalized Moonlight key packets to scan codes. Linux persists this preference but uses its fixed key mapping."), Amaterasu.field("Map Right Alt to Left Windows", keyRightAltToKeyWin, "Maps Moonlight Right Alt (VK_RMENU) to Left Windows (VK_LWIN) on Windows and Linux; a held key stays paired with its original release if this changes."), Amaterasu.field("Key repeat delay (milliseconds)", keyRepeatDelay, "Default is 500 milliseconds. Set to 0 to disable key repeat."), Amaterasu.field("Key repeat frequency (Hz)", keyRepeatFrequency, "Default is 24.9 Hz. Set to 0 to use the 24.9 Hz default; decimal frequencies are supported."), Amaterasu.field("Back button timeout (milliseconds)", backButtonTimeout, "On Linux when controller input is available, holding Back for this many milliseconds pulses Guide for 100 milliseconds. Any negative value disables it; other backends do not inject gamepads."), status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${settings.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, status, async () => {
                        try {
                            const updated = await context.api.post("input/settings_update", {
                                ...parseInputPolicy(keyboard.checked, mouse.checked, highResolutionScrolling.checked, alwaysSendScancodes.checked, keyRightAltToKeyWin.checked, keyRepeatDelay.valueAsNumber, keyRepeatFrequency.valueAsNumber, backButtonTimeout.valueAsNumber),
                                expected_revision: settings.revision,
                            });
                            context.notify(updated.applied
                                ? "Input policy saved and applied."
                                : "Input policy saved; native cleanup is pending.");
                        }
                        catch (error) {
                            if (!(error instanceof Amaterasu.ApiError) || error.code !== "cleanup_pending")
                                throw error;
                            context.notify("Input policy was saved, but releasing held native input is pending.");
                        }
                        await loadInput();
                    });
                });
                inputArea.replaceChildren(Amaterasu.panel("Input policy", form));
            }
            catch (error) {
                if (context.content.isConnected)
                    inputArea.replaceChildren(Amaterasu.errorPanel(error, loadInput));
            }
        }
        async function loadSteamRuntimeStatus(target, context) {
            try {
                const status = await context.api.post("steam/status");
                if (!target.isConnected)
                    return;
                target.textContent = steamStatusDescription(status);
            }
            catch (error) {
                if (!target.isConnected)
                    return;
                target.textContent =
                    error instanceof Amaterasu.ApiError && error.code === "forbidden"
                        ? "Runtime status requires steam.read; settings remain available with configuration access."
                        : "Current provider status is unavailable; settings remain editable.";
            }
        }
        async function loadHost() {
            hostArea.replaceChildren(Amaterasu.el("p", "muted", "Loading host name…"));
            try {
                const settings = await context.api.post("nvhttp/settings");
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
                        if (!context.content.isConnected)
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
                        if (!context.content.isConnected)
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
                form.append(Amaterasu.field("Name shown to Moonlight", hostname, "Leave blank to use this PC's name."), Amaterasu.field("Moonlight HTTP port", port, "HTTPS uses the port five below this value."), Amaterasu.field("Address family", addressFamily, "Choose IPv4 only, both families, or retain the startup policy."), Amaterasu.field("Use startup bind address", useStartupBindAddress, `The effective bind address is ${settings.bind_address}. Turn this off to choose an explicit address; a blank value is distinct from startup policy.`), Amaterasu.field("Bind address override", bindAddress, "Use a literal IPv4 or IPv6 address. An explicit blank uses the startup policy's wildcard/loopback behavior."), Amaterasu.field("Capture backend", capture, `Automatic uses the startup backend. The current backend is ${settings.capture_provider ?? "unavailable"}; explicit Windows modes fail if unavailable. Save and reopen settings to list outputs for a new backend.`), Amaterasu.field("Capture output", outputName, "Choose a display for future Moonlight streams. Automatic retains the configured startup output."), Amaterasu.field("Stream audio", streamAudio, "Enable host audio capture for future streams. Applications and paired clients may override this setting."), Amaterasu.field("WGC pacing smoothing", wgcPacing, `Preserve the frame pacing grid after eligible Windows WGC capture misses. Current effective policy: ${settings.effective_wgc_pacing_smoothing ? "enabled" : "disabled"}. Constant-rate WGC currently lacks fresh-frame identity for this policy.`), Amaterasu.field("Audio capture endpoint", audioSink, `Choose the output device used for loopback capture in future streams. Automatic uses the startup endpoint. Current effective endpoint: ${settings.effective_audio_sink}.`), Amaterasu.field("Capture selected audio endpoint without changing default outputs", audioSinkCaptureOnly, "For an explicitly selected Windows endpoint, keep the host's Console, Multimedia, and Communications default outputs. The setting applies to future streams."), Amaterasu.field("External IP", externalIp, "Stored for configuration parity. This host does not currently advertise or detect an external IP from this setting; blank resets it."), Amaterasu.field("Video batch size (KiB)", videoBatchSize, `Use 16, 32, or 64 KiB; blank or 0 selects 64 KiB. Current effective size: ${settings.effective_video_max_batch_size_kb} KiB${settings.video_batch_size_fallback ? " (configured value fell back to 64 KiB)" : ""}.`), Amaterasu.field("Maximum bitrate (kbps)", maxBitrate, `A positive value caps the host's initial stream bitrate. Blank resets to no host cap; zero or negative values also leave it uncapped. Caps below 100 kbps cannot launch with the current encoder. Current cap: ${settings.effective_max_bitrate_kbps === null ? "none" : `${settings.effective_max_bitrate_kbps} kbps`}.`), Amaterasu.field("Minimum frame rate (FPS)", minimumFpsTarget, `On idle capture, repeat the last accepted frame to target this rate. Zero uses the negotiated stream FPS; blank resets to 20 FPS. Current effective target: ${settings.effective_minimum_fps_target} FPS.`), Amaterasu.field("Stream ping timeout (ms)", pingTimeout, `Disconnect a stream after this much time without a ping. Zero expires immediately; blank resets to the default. Current effective timeout: ${settings.effective_ping_timeout_ms} ms.`), ...(outputDiscoveryIssue ? [Amaterasu.el("p", "muted", outputDiscoveryIssue)] : []), ...(audioDiscoveryIssue ? [Amaterasu.el("p", "muted", audioDiscoveryIssue)] : []), Amaterasu.el("p", "muted", `Active HTTP addresses: ${settings.http_addresses?.join(", ") || "off"} · HTTPS: ${settings.https_addresses?.join(", ") || "off"} · RTSP: ${settings.rtsp_addresses?.join(", ") || "off"}`), Amaterasu.el("p", "muted", `Active ports: HTTP ${settings.active_http_port ?? "off"}, HTTPS ${settings.active_https_port ?? "off"}`), ...(settings.listener_error ? [Amaterasu.el("p", "error", settings.listener_error)] : []), status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${settings.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, status, async () => {
                        const network = parseNvhttpNetworkOverrides(addressFamily.value, useStartupBindAddress.checked, bindAddress.value);
                        await context.api.post("nvhttp/settings_update", {
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
                            audio_sink_capture_only: audioSinkCaptureOnly.checked,
                            external_ip: parseExternalIp(externalIp.value),
                            wgc_pacing_smoothing: parseWgcPacingSmoothing(wgcPacing.value),
                            expected_revision: settings.revision,
                        });
                        context.notify("Host settings saved.");
                        await loadHost();
                    });
                });
                hostArea.replaceChildren(Amaterasu.panel("Moonlight host name", form));
            }
            catch (error) {
                if (context.content.isConnected)
                    hostArea.replaceChildren(Amaterasu.errorPanel(error, loadHost));
            }
        }
        async function load() {
            area.replaceChildren(Amaterasu.el("p", "muted", "Loading settings…"));
            try {
                const document = await context.api.post("configuration/get");
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
                form.append(status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${document.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, status, async () => {
                        const values = {};
                        for (const [name, control] of inputs) {
                            const definition = document.fields[name];
                            if (definition)
                                values[name] = Amaterasu.fieldValue(definition, control);
                        }
                        await context.api.post("configuration/update", {
                            values,
                            expected_revision: document.revision,
                        });
                        context.notify("Settings saved.");
                        await load();
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
            area.replaceChildren(hero, metrics, Amaterasu.panel("Service access", Amaterasu.table(["Service", "Status", "Details"], serviceRows)), Amaterasu.el("div", "notice", Amaterasu.el("strong", "", "Streaming is still being built"), Amaterasu.el("p", "", "Client pairing, capture, and stream connections are not available in this release. The management tools shown here are live.")));
        }
        void load();
    }
    Amaterasu.renderOverview = renderOverview;
    function renderSessions(context) {
        const area = Amaterasu.el("div");
        const status = Amaterasu.el("div");
        const savedArea = Amaterasu.el("div");
        const savedDetail = Amaterasu.el("div");
        const savedStatus = Amaterasu.el("div");
        let offset = 0;
        const history = [0];
        let savedOffset = 0;
        context.content.append(Amaterasu.sectionHeading("Sessions", "Inspect and stop sessions owned by this management console.", Amaterasu.button("Refresh", () => {
            void load();
            void loadSaved();
        })), status, area, Amaterasu.panel("Recorded stream sessions", savedArea, savedStatus, savedDetail));
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
            savedArea.replaceChildren(Amaterasu.el("p", "muted", "Loading recorded sessions…"));
            try {
                const reply = await context.api.post("history/list_sessions", { limit: 25, offset: savedOffset });
                if (!context.content.isConnected)
                    return;
                const rows = reply.sessions.map((session) => {
                    const details = Amaterasu.button("Details", () => loadSavedDetail(session.session_id));
                    const remove = Amaterasu.button("Delete", async () => {
                        if (!(await Amaterasu.confirmAction("Delete this recorded session?", `The history for ${session.app_name || session.session_id} will be removed.`, "Delete history")))
                            return;
                        await Amaterasu.runAction(remove, savedStatus, async () => {
                            await context.api.post("history/delete_session", {
                                session_id: session.session_id,
                            });
                            savedDetail.replaceChildren();
                            context.notify("Recorded session deleted.");
                            await loadSaved();
                        });
                    }, "quiet danger-text");
                    remove.disabled =
                        session.ended_at_unix == null || !context.api.allows("session.write");
                    return [
                        Amaterasu.el("div", "", Amaterasu.el("strong", "", session.app_name || session.session_id), Amaterasu.el("small", "muted block", session.client_name || session.protocol)),
                        new Date(session.started_at_unix * 1000).toLocaleString(),
                        session.ended_at_unix == null ? Amaterasu.badge("Active", "good") :
                            `${session.duration_seconds ?? 0} seconds`,
                        Amaterasu.el("div", "actions", details, remove),
                    ];
                });
                savedArea.replaceChildren(reply.sessions.length
                    ? Amaterasu.table(["Application / client", "Started", "Duration", "Actions"], rows)
                    : Amaterasu.empty("No recorded streams", "Completed streams appear here when history recording is enabled."), Amaterasu.pager(savedOffset > 0
                    ? () => {
                        savedOffset = Math.max(0, savedOffset - 25);
                        void loadSaved();
                    }
                    : null, reply.sessions.length === 25
                    ? () => {
                        savedOffset += 25;
                        void loadSaved();
                    }
                    : null, `Showing from ${savedOffset + 1}`));
            }
            catch (error) {
                if (context.content.isConnected)
                    savedArea.replaceChildren(Amaterasu.errorPanel(error, loadSaved));
            }
        }
        async function loadSavedDetail(sessionId) {
            savedDetail.replaceChildren(Amaterasu.el("p", "muted", "Loading session details…"));
            try {
                const reply = await context.api.post("history/session_detail", { session_id: sessionId });
                if (!context.content.isConnected)
                    return;
                const detail = reply.detail;
                if (!detail) {
                    savedDetail.replaceChildren(Amaterasu.empty("Session unavailable", "The recorded session was removed."));
                    return;
                }
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
                if (context.content.isConnected)
                    savedDetail.replaceChildren(Amaterasu.errorPanel(error, () => loadSavedDetail(sessionId)));
            }
        }
        void load();
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
    function renderSteam(context) {
        const area = Amaterasu.el("div");
        const status = Amaterasu.el("div");
        let revision;
        let after;
        const history = [undefined];
        const scan = Amaterasu.button("Scan libraries", () => Amaterasu.runAction(scan, status, async () => {
            await context.api.post("steam/refresh");
            revision = undefined;
            after = undefined;
            history.splice(0, history.length, undefined);
            context.notify("Steam scan requested. Refresh to see completed discoveries.");
            await load();
        }), "primary");
        scan.disabled = !context.api.allows("steam.refresh");
        context.content.append(Amaterasu.sectionHeading("Steam library", "Read-only discovery from the Steam roots configured on this host.", Amaterasu.button("Refresh", () => {
            revision = undefined;
            after = undefined;
            history.splice(0, history.length, undefined);
            return load();
        }), scan), status, area);
        async function load() {
            try {
                const [reply, provider] = await Promise.all([
                    context.api.post("steam/list", {
                        ...(revision != null ? { revision } : {}),
                        ...(after != null ? { after } : {}),
                        limit: 16,
                    }),
                    context.api.post("steam/status"),
                ]);
                if (!context.content.isConnected)
                    return;
                revision = reply.revision;
                scan.disabled =
                    !context.api.allows("steam.refresh") || provider.enabled === false;
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
    function renderDiagnostics(context) {
        const area = Amaterasu.el("div");
        let after = 0;
        const history = [0];
        context.content.append(Amaterasu.sectionHeading("Diagnostics", "Records and counters for this management console.", Amaterasu.button("Refresh", () => {
            after = 0;
            history.splice(0, history.length, 0);
            return load();
        })), area);
        async function load() {
            try {
                const [reply, counters] = await Promise.all([
                    context.api.post("diagnostics/query", {
                        after,
                        limit: 4,
                    }),
                    context.api.post("diagnostics/counters"),
                ]);
                if (!context.content.isConnected)
                    return;
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
                    : null, `Page ${history.length}`), Amaterasu.panel("Counters", Object.keys(counters.counters).length
                    ? Amaterasu.table(["Counter", "Value"], Object.entries(counters.counters).map(([name, value]) => [
                        name,
                        String(value),
                    ]))
                    : Amaterasu.el("p", "muted", "No counters have been recorded.")));
            }
            catch (error) {
                if (context.content.isConnected)
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
    const pageNames = {
        overview: "Overview",
        catalog: "Library",
        settings: "Settings",
        sessions: "Sessions",
        applications: "Applications",
        steam: "Steam",
        diagnostics: "Diagnostics",
    };
    const pageIcons = {
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
            window.addEventListener("pagehide", () => this.api.clear());
            window.addEventListener("pageshow", (event) => {
                if (event.persisted) {
                    this.api.clear();
                    this.loginScreen();
                }
            });
            this.loginScreen();
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
            const rememberField = Amaterasu.field("Remember this tab", rememberMe, "Use the longer refresh lifetime; tokens still stay in memory.");
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
                        this.api.useToken(token.value.trim());
                    else
                        await this.api.login(username.value.trim(), password.value, rememberMe.checked);
                    password.value = "";
                    token.value = "";
                    this.shell();
                });
            });
            const setup = Amaterasu.el("details", "setup-note", Amaterasu.el("summary", "", "First time setting up this host?"), Amaterasu.el("p", "", "An administrator must create the initial credentials through the trusted local console. Password setup is not available in this browser."), Amaterasu.el("p", "muted", "The management listener must be enabled and this browser origin allowed. Contact the person who configured the host."));
            card.append(form, setup, Amaterasu.el("div", "login-security", Amaterasu.el("span", "status-dot"), Amaterasu.el("span", "", "Tokens stay in this tab’s memory. Reloading signs you out.")));
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
                void this.api.logout();
                clearTimeout(this.notificationTimer);
                document
                    .querySelectorAll("dialog")
                    .forEach((dialog) => dialog.remove());
                this.loginScreen();
            }, "quiet");
            const top = Amaterasu.el("header", "topbar", Amaterasu.el("div", "actions", menu, Amaterasu.el("span", "topbar-label", "Host workspace")), Amaterasu.el("div", "account", Amaterasu.el("span", "account-avatar", this.api.subject.slice(0, 1).toUpperCase()), Amaterasu.el("span", "account-name", this.api.subject), logout));
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
                catalog: Amaterasu.renderCatalog,
                settings: Amaterasu.renderSettings,
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
