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
    plugins: "Plugins",
    sessions: "Sessions",
    applications: "Applications",
    steam: "Steam",
    diagnostics: "Diagnostics",
    accounts: "Accounts",
    tokens: "API tokens",
    maintenance: "Maintenance",
  };
  const pageIcons: Record<Page, string> = {
    overview: "M3 3h7v7H3z M14 3h7v7h-7z M3 14h7v7H3z M14 14h7v7h-7z",
    host: "M4 5h16v11H4z M8 20h8 M12 16v4",
    catalog: "M4 4h7v7H4z M13 4h7v7h-7z M4 13h7v7H4z M13 13h7v7h-7z",
    integrations: "M8 3v5 M16 3v5 M6 8h12v3a6 6 0 0 1-12 0z M12 17v4",
    settings: "M4 6h16 M4 12h16 M4 18h16 M8 4v4 M16 10v4 M10 16v4",
    plugins: "M4 4h7v7H4z M4 13h7v7H4z M13 13h7v7h-7z M16.5 4.5v6 M13.5 7.5h6",
    sessions: "M4 8h14 M15 5l3 3-3 3 M20 16H6 M9 13l-3 3 3 3",
    applications: "M4 5h16v14H4z M4 9h16 M8 7h.01",
    steam: "M12 3a9 9 0 1 1-8.5 12 M3 12l6 3 M9 15a2 2 0 1 0 4 0 2 2 0 0 0-4 0 M13 14l3-4 M18 8a2 2 0 1 1-4 0 2 2 0 0 1 4 0",
    diagnostics: "M3 12h4l2-6 4 12 2-6h6",
    maintenance: "M14 4a5 5 0 0 0-6 6L3 15l3 3 5-5a5 5 0 0 0 6-6l-3 3-3-3z",
    tokens: "M8 14a4 4 0 1 1 3.5-6H21v4h-3v3h-3v-3h-3.5A4 4 0 0 1 8 14z M7 10h.01",
    accounts: "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8z M4 20a8 8 0 0 1 16 0",
  };

  function navigationIcon(page: Page): SVGSVGElement {
    const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    icon.setAttribute("viewBox", "0 0 24 24");
    icon.setAttribute("class", "nav-icon");
    icon.setAttribute("aria-hidden", "true");
    icon.setAttribute("fill", "none");
    icon.setAttribute("stroke", "currentColor");
    icon.setAttribute("stroke-width", "1.7");
    icon.setAttribute("stroke-linecap", "round");
    icon.setAttribute("stroke-linejoin", "round");
    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    path.setAttribute("d", pageIcons[page]);
    icon.append(path);
    return icon;
  }

  const navigationGroups: { label: string; pages: Page[] }[] = [
    { label: "HOST", pages: ["overview", "host", "sessions", "diagnostics"] },
    { label: "LIBRARY", pages: ["catalog", "applications", "steam"] },
    { label: "SYSTEM", pages: ["settings", "integrations", "plugins", "accounts", "tokens", "maintenance"] },
  ];

  const corePages = new Map<Page, (context: PageContext) => void>([
    ["overview", renderOverview], ["host", renderHostManagement], ["catalog", renderCatalog],
    ["settings", renderSettings], ["integrations", renderIntegrations], ["plugins", renderPlugins], ["sessions", renderSessions],
    ["applications", renderApplications], ["steam", renderSteam], ["diagnostics", renderDiagnostics],
    ["accounts", renderAccounts], ["tokens", renderTokens], ["maintenance", renderMaintenance],
  ]);

  const pageScopes: Partial<Record<Page, string>> = {
    host: "configuration.read", catalog: "catalog.read", settings: "configuration.read",
    integrations: "configuration.read", plugins: "configuration.read", sessions: "session.read", applications: "application.read",
    steam: "steam.read", diagnostics: "diagnostics.read", accounts: "account.self",
  };

  class Application {
    private readonly api: Api;
    private readonly root: HTMLElement;
    private readonly theme: ThemeController;
    private signedIn = false;
    private content = el("main");
    private navigation = new Map<string, HTMLButtonElement>();
    private extensions: ExtensionPages;
    private extensionIds = new Set<string>();
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
          "",
          badge("LOCAL HOST MANAGEMENT", "accent"),
          el("h1", "", "Your host.\nWithin reach."),
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
      // The plugin manager used to be reached as "updates"; old links still work.
      const value = fragment.startsWith("catalog?") ? "catalog" : fragment === "updates" ? "plugins" : fragment;
      const core = Object.hasOwn(pageNames, value) && this.pageAvailable(value as Page);
      const settingsCategory = value.startsWith("settings/") &&
        ["general", "streaming", "integrations", "devices", "access"].includes(value.slice(9)) &&
        this.pageAvailable("settings");
      const pluginsView = value.startsWith("plugins/") &&
        (PLUGINS_VIEWS as readonly string[]).includes(value.slice(8)) &&
        this.pageAvailable("plugins");
      return core || settingsCategory || pluginsView ||
        (value.startsWith("extension/") && this.extensionIds.has(value.slice(10))) ? value : "overview";
    }

    private pageAvailable(page: Page): boolean {
      if (page === "tokens") return this.api.role === "administrator" && this.api.allows("tokens.manage");
      if (page === "maintenance") return this.api.role === "administrator" && this.api.allows("configuration.write");
      const scope = pageScopes[page];
      return !scope || this.api.allows(scope) || (page === "diagnostics" && this.api.allows("host-logs.read")) || (page === "accounts" && this.api.allows("configuration.write"));
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
      this.extensionIds.clear();
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
          "",
          badge("LOCAL HOST MANAGEMENT", "accent"),
          el("h1", "", "Your host.\nWithin reach."),
          el(
            "p",
            "",
            "A considered place to organize applications, manage sessions, and keep an eye on your host.",
          ),
        ),
        el("p", "login-footnote", "Independent components. One clear view."),
      );
      const card = el(
        "section",
        "login-card",
        el("p", "eyebrow", "WELCOME BACK"),
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

    private brand(): HTMLElement {
      return el(
        "div",
        "brand",
        el("span", "brand-symbol", el("span")),
        el("span", "", "amaterasu"),
      );
    }

    private shell(): void {
      this.signedIn = true;
      this.extensions.reset();
      this.extensionIds.clear();
      this.navigation.clear();
      const shell = el("div", "shell");
      const sidebar = el("aside", "sidebar");
      const navigationContainer = el("div", "navigation-container");
      const closeMenu = () => {
        shell.classList.remove("menu-open");
        menu.setAttribute("aria-expanded", "false");
      };
      for (const group of navigationGroups) {
        const pages = group.pages.filter((page) => this.pageAvailable(page));
        if (pages.length === 0) continue;
        const nav = el("nav", "navigation");
        setLocalizedAttribute(nav, "aria-label", group.label);
        for (const page of pages) {
          const item = button(pageNames[page], () => {
            closeMenu();
            this.navigate(page);
          }, "nav-item");
          item.prepend(navigationIcon(page));
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
          this.extensionIds.add(entry.id);
          const page = `extension/${entry.id}`;
          const item = button(appTextLiteral(entry.name), () => {
            shell.classList.remove("menu-open");
            menu.setAttribute("aria-expanded", "false");
            this.navigate(page);
          }, "nav-item");
          extensionsNav.append(item);
          this.navigation.set(page, item);
        }
        extensionsHeading.hidden = entries.length === 0;
        if (location.hash.startsWith("#extension/")) this.renderRequestedPage();
      }).catch(() => {
        if (this.signedIn && shell.isConnected) {
          extensionsHeading.textContent = "Extensions unavailable";
          extensionsHeading.hidden = false;
        }
      });
      sidebar.append(
        this.brand(),
        el("div", "sidebar-navigation", navigationContainer, extensionsHeading, extensionsNav,
          mountLocaleSelector(this.api)),
        el("div", "sidebar-bottom", this.theme.mount(), el("strong", "", "Host console"),
          el("p", "muted", "Manage this host and its devices")),
      );
      const menu = button(
        "☰",
        () => {
          const open = shell.classList.toggle("menu-open");
          menu.setAttribute("aria-expanded", String(open));
          if (open) sidebar.querySelector<HTMLButtonElement>(".nav-item")?.focus();
        },
        "menu-toggle",
      );
      setLocalizedAttribute(menu, "aria-label", "Toggle navigation");
      menu.setAttribute("aria-expanded", "false");
      sidebar.id = "main-navigation";
      menu.setAttribute("aria-controls", sidebar.id);
      const scrim = button("Close navigation", () => { closeMenu(); menu.focus(); }, "nav-scrim");
      shell.addEventListener("keydown", (event) => {
        if (event.key === "Escape" && shell.classList.contains("menu-open")) {
          closeMenu();
          menu.focus();
        } else if (event.key === "Tab" && shell.classList.contains("menu-open")) {
          const controls = Array.from(sidebar.querySelectorAll<HTMLElement>(
            "button:not(:disabled), select:not(:disabled), a[href]",
          )).filter((control) => control.getClientRects().length > 0);
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
      const top = el(
        "header",
        "topbar",
        el(
          "div",
          "actions",
          menu,
          el("span", "topbar-label", "Host workspace"),
        ),
        el(
          "div",
          "account",
          el(
            "span",
            "account-avatar",
            this.api.subject.slice(0, 1).toUpperCase(),
          ),
          el("span", "account-name", appTextLiteral(this.api.subject)),
          logout,
        ),
      );
      this.notification = el("div", "toast");
      this.notification.setAttribute("role", "status");
      this.notification.hidden = true;
      this.content = el("main", "content");
      this.content.id = "main";
      this.content.tabIndex = -1;
      shell.append(
        sidebar,
        scrim,
        el("div", "workspace", top, this.content),
        this.notification,
      );
      this.root.replaceChildren(shell);
      this.renderPage(this.currentPage());
    }

    private renderPage(page: string): void {
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
          (name === "plugins" && page.startsWith("plugins/"));
        item.classList.toggle("selected", selected);
        if (selected) item.setAttribute("aria-current", "page");
        else item.removeAttribute("aria-current");
      }
      const context: PageContext = {
        api: this.api,
        content: fresh,
        notify: (message) => this.notify(message),
        navigate: (page) => this.navigate(page),
        ...(page.startsWith("settings/") ? { initialSettingsCategory: page.slice(9) as SettingsCategory } : {}),
        replaceSettingsCategory: (category) => {
          if (!fresh.isConnected || !(this.displayedPage === "settings" || this.displayedPage?.startsWith("settings/"))) return;
          const next = `settings/${category}`;
          const address = new URL(location.href);
          address.hash = next;
          history.replaceState(history.state, "", address);
          this.displayedPage = next;
        },
        ...(page.startsWith("plugins/") ? { initialPluginsView: page.slice(8) as PluginsView } : {}),
        replacePluginsView: (view) => {
          if (!fresh.isConnected || !(this.displayedPage === "plugins" || this.displayedPage?.startsWith("plugins/"))) return;
          const next = view === "installed" ? "plugins" : `plugins/${view}`;
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
        fresh.append(el("p", "muted", "Loading extension…"));
        this.draftGuards.register(() => this.extensions.isDirty(), () => this.extensions.dispose());
        void this.extensions.mount(page.slice(10), fresh).catch((error) => {
          if (fresh.isConnected) fresh.replaceChildren(errorPanel(error));
        });
        setLocalizedPageTitle("Extension");
      } else {
        const corePage = (page.startsWith("settings/") ? "settings" : page.startsWith("plugins/") ? "plugins" : page) as Page;
        const renderer = corePages.get(corePage);
        if (!renderer) return;
        renderer(context);
        setLocalizedPageTitle(pageNames[corePage]);
      }
      fresh.focus({ preventScroll: true });
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
