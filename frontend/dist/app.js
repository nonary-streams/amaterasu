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
    function entryFromForm(data) {
        const text = (name) => String(data.get(name) ?? "").trim();
        const kind = text("kind") === "executable" ? "executable" : "application";
        const launch = kind === "application"
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
        return entry.launch.kind === "application"
            ? (entry.launch.path ?? "")
            : (entry.launch.program ?? "");
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
            const edit = Amaterasu.button("Edit", () => showEditor(row));
            edit.disabled = !api.allows("catalog.write");
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
            return Amaterasu.el("article", "app-card", Amaterasu.el("div", "app-art", Amaterasu.el("span", "", row.entry.name.slice(0, 2).toUpperCase()), Amaterasu.badge(row.entry.launch.kind === "executable"
                ? "Executable"
                : "Application")), Amaterasu.el("div", "app-details", Amaterasu.el("h2", "", row.entry.name), Amaterasu.el("p", "muted description", row.entry.description || "No description added."), Amaterasu.el("div", "tags", ...(row.entry.tags ?? []).map((tag) => Amaterasu.badge(tag))), Amaterasu.el("p", "path", Amaterasu.launchPath(row.entry)), Amaterasu.el("div", "card-footer", edit, remove)));
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
        const tags = Amaterasu.input("tags", entry?.tags.join(", ") ?? "");
        const kind = Amaterasu.el("select");
        kind.name = "kind";
        for (const [value, text] of [
            ["application", "Application / bundle"],
            ["executable", "Executable with arguments"],
        ]) {
            const option = Amaterasu.el("option", "", text);
            option.value = value ?? "";
            kind.append(option);
        }
        kind.value = entry?.launch.kind ?? "application";
        const path = Amaterasu.input("path", entry ? Amaterasu.launchPath(entry) : "");
        path.required = true;
        path.placeholder = "/Applications/Example.app";
        const argumentsField = Amaterasu.el("textarea");
        argumentsField.name = "arguments";
        argumentsField.rows = 2;
        argumentsField.value = JSON.stringify(entry?.launch.arguments ?? []);
        const directory = Amaterasu.input("working_directory", entry?.launch.working_directory ?? "");
        const executable = Amaterasu.el("div", "form-grid", Amaterasu.field("Arguments", argumentsField, 'A JSON array of literal arguments. Example: ["--fullscreen"]'), Amaterasu.field("Working directory", directory, "Optional absolute path."));
        const showKind = () => {
            executable.hidden = kind.value !== "executable";
            path.placeholder =
                kind.value === "executable"
                    ? "/opt/games/example"
                    : "/Applications/Example.app";
        };
        kind.addEventListener("change", showKind);
        showKind();
        form.append(Amaterasu.el("div", "form-grid", Amaterasu.field("Name", name), Amaterasu.field("Launch type", kind)), Amaterasu.field("Description", description), Amaterasu.field("Tags", tags, "Separate tags with commas."), Amaterasu.field("Absolute application path", path), executable);
        return form;
    }
})(Amaterasu || (Amaterasu = {}));
var Amaterasu;
(function (Amaterasu) {
    const maxConfiguredAuthTtlSeconds = 0x7fff_ffff;
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
        context.content.append(Amaterasu.sectionHeading("Settings", "Edit host options and values registered for this management console.", Amaterasu.button("Refresh", () => {
            void load();
            void loadAuth();
            void loadHost();
            void loadSteam();
        })), area, authArea, hostArea, steamArea);
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
                    control.min = "1";
                    control.max = String(maxConfiguredAuthTtlSeconds);
                    control.step = "1";
                }
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
                const status = Amaterasu.el("div");
                const save = Amaterasu.el("button", "button primary", "Save host settings");
                save.type = "submit";
                save.disabled = !context.api.allows("configuration.write");
                form.append(Amaterasu.field("Name shown to Moonlight", hostname, "Leave blank to use this PC's name."), Amaterasu.field("Moonlight HTTP port", port, "HTTPS uses the port five below this value."), Amaterasu.field("Address family", addressFamily, "Choose IPv4 only, both families, or retain the startup policy."), Amaterasu.field("Use startup bind address", useStartupBindAddress, `The effective bind address is ${settings.bind_address}. Turn this off to choose an explicit address; a blank value is distinct from startup policy.`), Amaterasu.field("Bind address override", bindAddress, "Use a literal IPv4 or IPv6 address. An explicit blank uses the startup policy's wildcard/loopback behavior."), Amaterasu.el("p", "muted", `Active HTTP addresses: ${settings.http_addresses?.join(", ") || "off"} · HTTPS: ${settings.https_addresses?.join(", ") || "off"} · RTSP: ${settings.rtsp_addresses?.join(", ") || "off"}`), Amaterasu.el("p", "muted", `Active ports: HTTP ${settings.active_http_port ?? "off"}, HTTPS ${settings.active_https_port ?? "off"}`), ...(settings.listener_error ? [Amaterasu.el("p", "error", settings.listener_error)] : []), status, Amaterasu.el("div", "actions", save, Amaterasu.badge(`Revision ${settings.revision}`)));
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    void Amaterasu.runAction(save, status, async () => {
                        const network = parseNvhttpNetworkOverrides(addressFamily.value, useStartupBindAddress.checked, bindAddress.value);
                        await context.api.post("nvhttp/settings_update", {
                            hostname: hostname.value,
                            port: port.valueAsNumber,
                            ...network,
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
        let offset = 0;
        const history = [0];
        context.content.append(Amaterasu.sectionHeading("Sessions", "Inspect and stop sessions owned by this management console.", Amaterasu.button("Refresh", load)), status, area);
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
        void load();
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
