namespace Amaterasu {
  function appTextLiteral(value: string): TextValue {
    return typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(value) : value;
  }

  const pageNames: Record<Page, string> = {
    overview: "Overview",
    host: "Host & devices",
    catalog: "Library",
    settings: "Settings",
    integrations: "Integrations",
    plugins: "Modules",
    sessions: "Sessions",
    diagnostics: "Diagnostics",
    accounts: "Accounts",
    tokens: "API tokens",
    maintenance: "Maintenance",
  };
  const shellIcons: Record<Page | "menu", string> = {
    overview: "M3 4h8v8H3z M13 4h8v5h-8z M3 14h8v6H3z M13 11h8v9h-8z",
    host: "M2 5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v11H2z M5 6v7h14V6z M10 17h4v2h4v2H6v-2h4z",
    catalog: "M3 3h4v16H3z M9 3h5v16H9z M16 4l3-1 4 15-3 1z M2 20h21v2H2z",
    integrations: "M7 2h3v5h4V2h3v5h3v3h-2v2a6 6 0 0 1-4.5 5.8V22h-3v-4.2A6 6 0 0 1 6 12v-2H4V7h3z",
    settings: "M2 4h5V2h3v2h12v3H10v2H7V7H2z M2 10.5h12v-2h3v2h5v3h-5v2h-3v-2H2z M2 17h6v-2h3v2h11v3H11v2H8v-2H2z",
    plugins: "M3 4h8v7H3z M3 13h8v8H3z M13 13h8v8h-8z M17 1l6 6-6 5-5-5z",
    sessions: "M2 6h13V2l7 6-7 6v-4H2z M22 14v4H9v4l-7-6 7-6v4z",
    diagnostics: "M2 3h20v18H2z M4 13h4l2-4 3 8 3-6h4V9h-5l-1.6 3L10 5l-3 6H4z",
    maintenance: "M20.8 2.9l-4.1 4.2-3.1-3.1 4.1-4A7 7 0 0 0 9 8.6l-7.3 7.2a3.7 3.7 0 0 0 5.3 5.3l7.2-7.3a7 7 0 0 0 6.6-10.9z M5.3 17a1.3 1.3 0 1 1 0 2.6 1.3 1.3 0 0 1 0-2.6z",
    tokens: "M8 3a6 6 0 0 1 5.7 4H22v4h-2v3h-3v-3h-3.3A6 6 0 1 1 8 3z M6 7a2 2 0 1 0 0 4 2 2 0 0 0 0-4z",
    accounts: "M12 2a5 5 0 1 1 0 10 5 5 0 0 1 0-10z M3 21a9 9 0 0 1 18 0z",
    menu: "M3 5h18v3H3z M3 11h18v3H3z M3 17h18v3H3z",
  };

  function shellIcon(name: Page | "menu"): SVGSVGElement {
    const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    icon.setAttribute("viewBox", "0 0 24 24");
    icon.setAttribute("class", "nav-icon");
    icon.setAttribute("aria-hidden", "true");
    icon.setAttribute("focusable", "false");
    icon.setAttribute("fill", "currentColor");
    icon.setAttribute("fill-rule", "evenodd");
    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    path.setAttribute("d", shellIcons[name]);
    icon.append(path);
    return icon;
  }

  const navigationGroups: { label: string; pages: Page[] }[] = [
    { label: "HOST", pages: ["overview", "host", "sessions", "diagnostics"] },
    { label: "LIBRARY", pages: ["catalog"] },
    { label: "SYSTEM", pages: ["settings", "integrations", "plugins", "accounts", "tokens", "maintenance"] },
  ];

  const corePages = new Map<Page, (context: PageContext) => void>([
    ["overview", renderOverview], ["host", renderHostManagement], ["catalog", renderLibrary],
    ["settings", renderSettings], ["integrations", renderIntegrations], ["plugins", renderPlugins], ["sessions", renderSessions],
    ["diagnostics", renderDiagnostics],
    ["accounts", renderAccounts], ["tokens", renderTokens], ["maintenance", renderMaintenance],
  ]);

  const pageScopes: Partial<Record<Page, string>> = {
    host: "configuration.read", catalog: "catalog.read", settings: "configuration.read",
    integrations: "configuration.read", plugins: "configuration.read", sessions: "session.read",
    diagnostics: "diagnostics.read", accounts: "account.self",
  };

  class Application {
    private readonly api: Api;
    private readonly root: HTMLElement;
    private readonly theme: ThemeController;
    private signedIn = false;
    private content = el("main");
    private readonly mastheadContext = el("p", "console-masthead-context");
    private navigation = new Map<string, HTMLButtonElement>();
    private extensions: ExtensionPages;
    private extensionPageNames = new Map<string, string>();
    private notification = el("div");
    private notificationTimer = 0;
    private restorationGeneration = 0;
    private displayedPage: string | null = null;
    private draftGuards = new DraftGuardGroup();

    constructor() {
      this.root = document.getElementById("app")!;
      this.theme = new ThemeController();
      document
        .querySelector(".skip-link")
        ?.addEventListener("click", (event) => {
          event.preventDefault();
          const main = document.getElementById("main");
          main?.focus();
          main?.scrollIntoView();
        });
      this.api = new Api(
        document.querySelector<HTMLMetaElement>(
          'meta[name="amaterasu-api-base"]',
        )?.content ?? "",
      );
      this.extensions = new ExtensionPages(this.api);
      this.api.onAuthenticationLost = () => {
        if (this.signedIn) {
          clearTimeout(this.notificationTimer);
          document.querySelectorAll("dialog").forEach((dialog) => dialog.remove());
          this.loginScreen();
        }
      };
      window.addEventListener("hashchange", (event) => {
        if (this.signedIn) this.renderRequestedPage(event.oldURL);
      });
      window.addEventListener("beforeunload", (event) => {
        if (this.signedIn && this.draftGuards.isDirty()) {
          event.preventDefault();
          event.returnValue = "";
        }
      });
      window.addEventListener("resize", () => {
        if (window.innerWidth > 800) {
          this.root.querySelector(".shell")?.classList.remove("menu-open");
          this.root.querySelector(".menu-toggle")?.setAttribute("aria-expanded", "false");
        }
      });
      window.addEventListener("pagehide", () => this.api.suspend());
      window.addEventListener("pageshow", (event) => {
        if (event.persisted) {
          void this.restoreRememberedSession();
        }
      });
      void this.restoreRememberedSession();
    }

    private async restoreRememberedSession(): Promise<void> {
      const generation = ++this.restorationGeneration;
      this.sessionRestoringScreen();
      let restored = false;
      try {
        restored = await this.api.restoreSession();
      } catch {
        // An unavailable or expired remembered session returns to sign-in.
      }
      if (generation !== this.restorationGeneration) return;
      if (restored) this.shell();
      else this.loginScreen();
    }

    private sessionRestoringScreen(): void {
      this.releaseDraftGuard();
      this.displayedPage = null;
      this.signedIn = false;
      const main = el("main", "login-layout");
      main.id = "main";
      main.tabIndex = -1;
      main.setAttribute("aria-busy", "true");
      const intro = el(
        "section",
        "login-intro",
        this.brand(),
        el(
          "div",
          "login-intro-copy",
          el("h1", "", "Host console"),
          el("p", "", "Checking for a remembered session."),
        ),
      );
      const status = el(
        "section",
        "login-card",
        el("p", "eyebrow", "WELCOME BACK"),
        el("h2", "", "Restoring your session"),
        el("p", "muted", "Please wait while we check this browser."),
      );
      status.setAttribute("role", "status");
      status.setAttribute("aria-live", "polite");
      status.setAttribute("aria-atomic", "true");
      main.append(intro, el("div", "login-side", status));
      this.root.replaceChildren(main);
      setLocalizedPageTitle("Restoring session");
    }

    private currentPage(): string {
      const fragment = location.hash.slice(1);
      // The Modules page keeps the address it had as Plugins, and was reached
      // as "updates" before that; old links still work.
      const value = libraryRouteFromAddress(fragment) ?? (fragment === "updates" ? "plugins" : fragment);
      const core = Object.hasOwn(pageNames, value) && this.pageAvailable(value as Page);
      const settingsCategory = value.startsWith("settings/") &&
        ["everyday", "general", "streaming", "integrations", "devices", "access", "plugins"].includes(value.slice(9)) &&
        this.pageAvailable("settings");
      const pluginsView = value.startsWith("plugins/") &&
        pluginsViewFromAddress(value.slice(8)) !== null &&
        this.pageAvailable("plugins");
      const libraryView = libraryViewFromAddress(value);
      const library = libraryView !== null && libraryViewAvailable(this.api, libraryView);
      return core || settingsCategory || pluginsView || library ||
        (value.startsWith("extension/") && this.extensionPageNames.has(value.slice(10))) ? value : "overview";
    }

    private pageAvailable(page: Page): boolean {
      if (page === "catalog") return libraryViewAvailable(this.api, "games") || libraryViewAvailable(this.api, "integrations");
      if (page === "tokens") return this.api.role === "administrator" && this.api.allows("tokens.manage");
      if (page === "maintenance") return this.api.role === "administrator" && this.api.allows("configuration.write");
      const scope = pageScopes[page];
      return !scope || this.api.allows(scope) || (page === "diagnostics" && (this.api.allows("host-logs.read") || this.api.allows("session.read"))) || (page === "accounts" && this.api.allows("configuration.write"));
    }

    private releaseDraftGuard(): void {
      this.draftGuards.dispose();
    }

    private mayLeaveDraft(): boolean {
      return !this.draftGuards.isDirty() || window.confirm("Discard your unsaved changes?");
    }

    private renderRequestedPage(previousUrl?: string): void {
      const page = this.currentPage();
      if (page === this.displayedPage) return;
      if (!this.mayLeaveDraft()) {
        if (this.displayedPage) {
          const previous = new URL(location.href);
          const previousFragment = previousUrl ? new URL(previousUrl).hash.slice(1) : "";
          previous.hash = previousFragment === this.displayedPage ||
            (this.displayedPage === "catalog" && previousFragment.startsWith("catalog?"))
            ? previousFragment : this.displayedPage;
          history.replaceState(history.state, "", previous);
        }
        return;
      }
      this.renderPage(page);
    }

    private navigate(page: string): void {
      if (page !== this.displayedPage) location.hash = page;
    }

    private loginScreen(): void {
      this.releaseDraftGuard();
      this.displayedPage = null;
      this.signedIn = false;
      this.extensions.reset();
      this.extensionPageNames.clear();
      const main = el("main", "login-layout");
      main.id = "main";
      main.tabIndex = -1;
      const brand = this.brand();
      const intro = el(
        "section",
        "login-intro",
        brand,
        el(
          "div",
          "login-intro-copy",
          el("h1", "", "Host console"),
          el(
            "p",
            "",
            "Your applications, sessions, and local services in one place.",
          ),
        ),
      );
      const card = el(
        "section",
        "login-card",
        el("h2", "", "Connect to your host"),
        el(
          "p",
          "muted",
          "Sign in with your host account.",
        ),
      );
      const username = input("username");
      username.autocomplete = "username";
      username.required = true;
      const password = input("password", "", "password");
      password.autocomplete = "current-password";
      password.required = true;
      const rememberMe = input("remember_me", "", "checkbox");
      const rememberField = field(
        "Remember this browser",
        rememberMe,
        this.api.usesSecureCookies
          ? "Keep this signed-in browser session across restarts. Session cookies are protected from scripts."
          : "Store raw tokens in one localStorage record across reloads and restarts. Same-origin scripts can read it. Remember me requires Web Locks in a secure browser context; sign in without it when unavailable.",
      );
      const token = input("token", "", "password");
      token.autocomplete = "off";
      token.spellcheck = false;
      const credentials = el(
        "div",
        "login-fields",
        field("Username", username),
        field("Password", password),
      );
      const tokenField = field(
        "API token",
        token,
        "Token scope is enforced by the host on every request.",
      );
      tokenField.hidden = true;
      const status = el("div");
      status.setAttribute("aria-live", "polite");
      const submit = el("button", "button primary full-width", "Sign in →");
      submit.type = "submit";
      let tokenMode = false;
      const toggle = button(
        "Use an API token instead",
        () => {
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
          setLocalizedTextContent(
            toggle,
            tokenMode ? "Use username and password" : "Use an API token instead",
          );
          (tokenMode ? token : username).focus();
        },
        "link-button",
      );
      const form = el(
        "form",
        "",
        credentials,
        rememberField,
        tokenField,
        status,
        submit,
        toggle,
      );
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        if (!form.reportValidity()) return;
        void runAction(submit, status, async () => {
          if (tokenMode) await this.api.useToken(token.value.trim());
          else
            await this.api.login(
              username.value.trim(),
              password.value,
              rememberMe.checked,
            );
          password.value = "";
          token.value = "";
          this.shell();
        });
      });
      const setupCode = input("enrollment_code"); setupCode.required = true; setupCode.autocomplete = "off";
      const setupUsername = input("enrollment_username"); setupUsername.required = true; setupUsername.autocomplete = "username";
      const setupPassword = input("enrollment_password", "", "password"); setupPassword.required = true; setupPassword.autocomplete = "new-password";
      const setupStatus = el("div");
      setupStatus.setAttribute("role", "status");
      setupStatus.setAttribute("aria-live", "polite");
      setupStatus.setAttribute("aria-atomic", "true");
      const setupSubmit = el("button", "button primary", "Create administrator"); setupSubmit.type = "submit";
      const setupForm = el("form", "settings-form", field("Code shown on the host", setupCode), field("Username", setupUsername), field("Password", setupPassword), setupSubmit, setupStatus);
      setupForm.addEventListener("submit", (event) => {
        event.preventDefault(); if (!setupForm.reportValidity()) return;
        void runAction(setupSubmit, setupStatus, async () => {
          await this.api.post("auth/enroll", { code: setupCode.value.trim(), username: setupUsername.value.trim(), password: setupPassword.value }, false);
          username.value = setupUsername.value.trim(); setupCode.value = ""; setupPassword.value = "";
          setupStatus.replaceChildren(el("p", "", "Administrator created. Sign in above."));
        });
      });
      const setup = el("details", "setup-note", el("summary", "", "First time setting up this host?"),
        el("p", "", "Enter the enrollment code displayed locally on the host to create the first administrator."), setupForm);
      card.append(
        form,
        setup,
        el(
          "div",
          "login-security",
          el("span", "status-dot"),
          el("span", "", this.api.usesSecureCookies
            ? "Your browser sends protected session cookies only to this HTTPS host."
            : "Same-origin scripts can read remembered tokens; localStorage is not HttpOnly protection or a native-code sandbox."),
        ),
      );
      main.append(intro, el("div", "login-side", card));
      this.root.replaceChildren(main);
      username.focus();
      setLocalizedPageTitle("Sign in");
    }

    private brand(context?: HTMLElement): HTMLElement {
      const wordmark = el("span", "", "Amaterasu");
      return el("div", "brand", el("span", "brand-symbol"),
        context ? el("div", "console-brand-copy", wordmark, context) : wordmark);
    }

    private shell(): void {
      this.signedIn = true;
      this.extensions.reset();
      this.extensionPageNames.clear();
      this.navigation.clear();
      const shell = el("div", "shell console-shell");
      const sidebar = el("aside", "sidebar");
      const navigationContainer = el("div", "navigation-container");
      const closeMenu = () => {
        shell.classList.remove("menu-open");
        menu.setAttribute("aria-expanded", "false");
      };
      const choosePage = (page: string) => {
        if (page === this.displayedPage && shell.classList.contains("menu-open")) {
          closeMenu();
          menu.focus();
        }
        this.navigate(page);
      };
      for (const group of navigationGroups) {
        const pages = group.pages.filter((page) => this.pageAvailable(page));
        if (pages.length === 0) continue;
        const nav = el("nav", "navigation");
        setLocalizedAttribute(nav, "aria-label", group.label);
        for (const page of pages) {
          const item = button(pageNames[page], () => choosePage(page), "nav-item");
          item.prepend(shellIcon(page));
          nav.append(item);
          this.navigation.set(page, item);
        }
        navigationContainer.append(el("p", "nav-heading", group.label), nav);
      }
      const extensionsNav = el("nav", "navigation");
      setLocalizedAttribute(extensionsNav, "aria-label", "Extensions");
      const extensionsHeading = el("p", "nav-heading", "EXTENSIONS");
      extensionsHeading.hidden = true;
      void this.extensions.discover().then((entries) => {
        if (!this.signedIn || !shell.isConnected) return;
        for (const entry of entries) {
          if (!(entry.placements ?? ["page"]).includes("page")) continue;
          this.extensionPageNames.set(entry.id, entry.name);
          const page = `extension/${entry.id}`;
          const item = button(appTextLiteral(entry.name), () => choosePage(page), "nav-item");
          item.prepend(shellIcon("plugins"));
          extensionsNav.append(item);
          this.navigation.set(page, item);
        }
        extensionsHeading.hidden = this.extensionPageNames.size === 0;
        if (location.hash.startsWith("#extension/")) this.renderRequestedPage();
      }).catch(() => {
        if (this.signedIn && shell.isConnected) {
          extensionsHeading.textContent = "Extensions unavailable";
          extensionsHeading.hidden = false;
        }
      });
      const menu = button(
        "",
        () => {
          const open = shell.classList.toggle("menu-open");
          menu.setAttribute("aria-expanded", String(open));
          if (open) sidebar.querySelector<HTMLButtonElement>(".nav-item")?.focus();
        },
        "menu-toggle",
      );
      menu.append(shellIcon("menu"));
      setLocalizedAttribute(menu, "aria-label", "Toggle navigation");
      menu.setAttribute("aria-expanded", "false");
      sidebar.id = "main-navigation";
      menu.setAttribute("aria-controls", sidebar.id);
      const scrim = button("Close navigation", () => { closeMenu(); menu.focus(); }, "nav-scrim");
      scrim.tabIndex = -1;
      shell.addEventListener("keydown", (event) => {
        if (event.key === "Escape" && shell.classList.contains("menu-open")) {
          closeMenu();
          menu.focus();
        } else if (event.key === "Tab" && shell.classList.contains("menu-open")) {
          const controls = Array.from(sidebar.querySelectorAll<HTMLElement>(
            "button:not(:disabled), select:not(:disabled), summary, a[href]",
          )).filter((control) => control.getClientRects().length > 0 &&
            !control.matches("details:not([open]) :not(summary)"));
          const first = controls[0];
          const last = controls.at(-1);
          if (event.shiftKey && document.activeElement === first) {
            event.preventDefault();
            last?.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first?.focus();
          }
        }
      });
      const logout = button(
        "Sign out",
        () => {
          if (!this.mayLeaveDraft()) return;
          logout.disabled = true;
          void this.api.logout().then(() => {
            clearTimeout(this.notificationTimer);
            document.querySelectorAll("dialog").forEach((dialog) => dialog.remove());
            this.loginScreen();
          }).catch((error) => {
            const message = error instanceof Error
              ? error.message
              : "Could not confirm sign-out. Retry when the host is reachable.";
            if (
              error instanceof ApiError && error.code === "storage_unavailable" &&
              this.api.subject
            ) {
              logout.disabled = false;
              this.notify(message);
              return;
            }
            clearTimeout(this.notificationTimer);
            document.querySelectorAll("dialog").forEach((dialog) => dialog.remove());
            this.loginScreen();
            const warning = el("p", "error", appTextLiteral(message));
            warning.setAttribute("role", "alert");
            this.root.querySelector(".login-card")?.prepend(warning);
          });
        },
        "quiet",
      );
      const avatar = el("span", "account-avatar", appTextLiteral(this.api.subject.slice(0, 1).toUpperCase()));
      avatar.setAttribute("aria-hidden", "true");
      const account = el(
        "details",
        "console-account",
        el("summary", "account", avatar, el("span", "account-name", appTextLiteral(this.api.subject))),
        el("div", "console-account-utilities", mountLocaleSelector(this.api), this.theme.mount(), logout),
      );
      sidebar.append(
        el("div", "sidebar-navigation", navigationContainer, extensionsHeading, extensionsNav, account),
        el("div", "sidebar-bottom"),
      );
      this.mastheadContext.id = "console-route-title";
      const masthead = el("header", "console-masthead", menu, this.brand(this.mastheadContext));
      this.notification = el("div", "toast");
      this.notification.setAttribute("role", "status");
      this.notification.hidden = true;
      this.content = el("main", "content");
      this.content.id = "main";
      this.content.tabIndex = -1;
      shell.append(
        masthead,
        el("div", "console-columns", sidebar, scrim, el("div", "workspace", this.content)),
        this.notification,
      );
      this.root.replaceChildren(shell);
      this.renderPage(this.currentPage());
    }

    private renderPage(page: string): void {
      const focusLibraryTab = this.displayedPage !== null && libraryViewFromAddress(this.displayedPage) !== null &&
        libraryViewFromAddress(page) !== null && document.activeElement?.getAttribute("role") === "tab";
      this.releaseDraftGuard();
      this.displayedPage = page;
      clearTimeout(this.notificationTimer);
      this.notification.hidden = true;
      this.root.querySelector(".shell")?.classList.remove("menu-open");
      this.root.querySelector(".menu-toggle")?.setAttribute("aria-expanded", "false");
      this.extensions.dispose();
      const fresh = el("main", "content");
      fresh.id = "main";
      fresh.tabIndex = -1;
      this.content.replaceWith(fresh);
      this.content = fresh;
      for (const [name, item] of this.navigation) {
        const selected = name === page || (name === "settings" && page.startsWith("settings/")) ||
          (name === "plugins" && page.startsWith("plugins/")) ||
          (name === "catalog" && page.startsWith("catalog/"));
        item.classList.toggle("selected", selected);
        if (selected) item.setAttribute("aria-current", "page");
        else item.removeAttribute("aria-current");
      }
      const context: PageContext = {
        api: this.api,
        content: fresh,
        notify: (message) => this.notify(message),
        navigate: (page) => this.navigate(page),
        ...(libraryViewFromAddress(page) !== null ? { initialLibraryView: libraryViewFromAddress(page)! } : {}),
        ...(page.startsWith("settings/") ? { initialSettingsCategory: page.slice(9) as SettingsCategory } : {}),
        replaceSettingsCategory: (category) => {
          if (!fresh.isConnected || !(this.displayedPage === "settings" || this.displayedPage?.startsWith("settings/"))) return;
          const next = `settings/${category}`;
          const address = new URL(location.href);
          address.hash = next;
          history.replaceState(history.state, "", address);
          this.displayedPage = next;
        },
        ...(page.startsWith("plugins/")
          ? { initialPluginsView: pluginsViewFromAddress(page.slice(8)) ?? DEFAULT_PLUGINS_VIEW }
          : {}),
        replacePluginsView: (view) => {
          if (!fresh.isConnected || !(this.displayedPage === "plugins" || this.displayedPage?.startsWith("plugins/"))) return;
          const next = view === DEFAULT_PLUGINS_VIEW ? "plugins" : `plugins/${view}`;
          const address = new URL(location.href);
          address.hash = next;
          history.replaceState(history.state, "", address);
          this.displayedPage = next;
        },
        registerDraftGuard: (isDirty, dispose) => {
          if (fresh.isConnected) return this.draftGuards.register(isDirty, dispose);
          dispose();
          return () => {};
        },
      };
      if (page.startsWith("extension/")) {
        const id = page.slice(10);
        const name = this.extensionPageNames.get(id);
        this.mastheadContext.replaceChildren(textValueNode(name === undefined ? "Extension" : appTextLiteral(name)));
        fresh.setAttribute("aria-labelledby", this.mastheadContext.id);
        const loading = el("p", "muted", "Loading extension…");
        fresh.append(loading);
        this.draftGuards.register(() => this.extensions.isDirty(), () => this.extensions.dispose());
        void this.extensions.mount(id, fresh).then(() => loading.remove()).catch((error) => {
          if (fresh.isConnected) fresh.replaceChildren(errorPanel(error));
        });
        setLocalizedPageTitle("Extension");
      } else {
        const corePage = (page.startsWith("settings/") ? "settings" : page.startsWith("plugins/") ? "plugins" : page.startsWith("catalog/") ? "catalog" : page) as Page;
        const renderer = corePages.get(corePage);
        if (!renderer) return;
        setLocalizedTextContent(this.mastheadContext, pageNames[corePage]);
        renderer(context);
        setLocalizedPageTitle(pageNames[corePage]);
      }
      const selectedLibraryTab = focusLibraryTab ? fresh.querySelector<HTMLElement>('.library-tabs [aria-selected="true"]') : null;
      (selectedLibraryTab ?? fresh).focus({ preventScroll: true });
      window.scrollTo(0, 0);
    }

    private notify(message: TextValue): void {
      if (typeof message === "string") {
        this.notification.textContent = message;
      } else {
        this.notification.replaceChildren(textValueNode(message));
      }
      this.notification.hidden = false;
      clearTimeout(this.notificationTimer);
      this.notificationTimer = window.setTimeout(() => {
        this.notification.hidden = true;
      }, 5000);
    }
  }
  new Application();
}
