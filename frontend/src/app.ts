namespace Amaterasu {
  function appTextLiteral(value: string): TextValue {
    return typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(value) : value;
  }

  const pageNames: Record<Page, string> = {
    overview: "Overview",
    host: "Host & devices",
    catalog: "Library",
    settings: "Settings",
    updates: "Modules & UI",
    sessions: "Sessions",
    applications: "Applications",
    steam: "Steam",
    diagnostics: "Diagnostics",
    accounts: "Accounts",
  };
  const pageIcons: Record<Page, string> = {
    updates: "+",
    host: "⌂",
    overview: "▦",
    catalog: "⊞",
    settings: "≡",
    sessions: "⇄",
    applications: "□",
    steam: "◇",
    diagnostics: "⌁",
    accounts: "♙",
  };

  const corePages = new Map<Page, (context: PageContext) => void>([
    ["overview", renderOverview], ["host", renderHostManagement], ["catalog", renderCatalog],
    ["settings", renderSettings], ["updates", renderUpdates], ["sessions", renderSessions],
    ["applications", renderApplications], ["steam", renderSteam], ["diagnostics", renderDiagnostics],
    ["accounts", renderAccounts],
  ]);

  const pageScopes: Partial<Record<Page, string>> = {
    host: "configuration.read", catalog: "catalog.read", settings: "configuration.read",
    updates: "configuration.read", sessions: "session.read", applications: "application.read",
    steam: "steam.read", diagnostics: "diagnostics.read", accounts: "account.self",
  };

  class Application {
    private readonly api: Api;
    private readonly root: HTMLElement;
    private signedIn = false;
    private content = el("main");
    private navigation = new Map<string, HTMLButtonElement>();
    private extensions: ExtensionPages;
    private extensionIds = new Set<string>();
    private notification = el("div");
    private notificationTimer = 0;
    private restorationGeneration = 0;

    constructor() {
      this.root = document.getElementById("app")!;
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
      window.addEventListener("hashchange", () => {
        if (this.signedIn) this.renderPage(this.currentPage());
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
      const value = location.hash.slice(1);
      const core = Object.hasOwn(pageNames, value) && this.pageAvailable(value as Page);
      return core || (value.startsWith("extension/") && this.extensionIds.has(value.slice(10))) ? value : "overview";
    }

    private pageAvailable(page: Page): boolean {
      const scope = pageScopes[page];
      return !scope || this.api.allows(scope) || (page === "accounts" && this.api.allows("configuration.write"));
    }

    private navigate(page: string): void {
      if (location.hash === `#${page}`) this.renderPage(page);
      else location.hash = page;
    }

    private loginScreen(): void {
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
      const nav = el("nav", "navigation");
      setLocalizedAttribute(nav, "aria-label", "Main navigation");
      for (const page of corePages.keys()) {
        if (!this.pageAvailable(page)) continue;
        const icon = el("span", "nav-icon", pageIcons[page]);
        icon.setAttribute("aria-hidden", "true");
        const item = button(
          pageNames[page],
          () => {
            shell.classList.remove("menu-open");
            menu.setAttribute("aria-expanded", "false");
            this.navigate(page);
          },
          "nav-item",
        );
        item.prepend(icon);
        nav.append(item);
        this.navigation.set(page, item);
      }
      const extensionsNav = el("nav", "navigation");
      setLocalizedAttribute(extensionsNav, "aria-label", "Extensions");
      const extensionsHeading = el("p", "nav-heading", "EXTENSIONS");
      extensionsHeading.hidden = true;
      void this.extensions.discover().then((entries) => {
        if (!this.signedIn || !shell.isConnected) return;
        for (const entry of entries) {
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
        if (location.hash.startsWith("#extension/")) this.renderPage(this.currentPage());
      }).catch(() => {
        if (this.signedIn && shell.isConnected) {
          extensionsHeading.textContent = "Extensions unavailable";
          extensionsHeading.hidden = false;
        }
      });
      sidebar.append(
        this.brand(),
        el("p", "nav-heading", "WORKSPACE"),
        nav,
        extensionsHeading,
        extensionsNav,
        el(
          "div",
          "sidebar-bottom",
          badge("Development preview"),
          el("p", "muted", "Local management console"),
          el("small", "muted", "Streaming features are in progress."),
        ),
      );
      const menu = button(
        "☰",
        () => {
          const open = shell.classList.toggle("menu-open");
          menu.setAttribute("aria-expanded", String(open));
        },
        "menu-toggle",
      );
      setLocalizedAttribute(menu, "aria-label", "Toggle navigation");
      menu.setAttribute("aria-expanded", "false");
      const logout = button(
        "Sign out",
        () => {
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
          mountLocaleSelector(this.api),
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
        el("div", "workspace", top, this.content),
        this.notification,
      );
      this.root.replaceChildren(shell);
      this.renderPage(this.currentPage());
    }

    private renderPage(page: string): void {
      this.extensions.dispose();
      const fresh = el("main", "content");
      fresh.id = "main";
      fresh.tabIndex = -1;
      this.content.replaceWith(fresh);
      this.content = fresh;
      for (const [name, item] of this.navigation) {
        item.classList.toggle("selected", name === page);
        if (name === page) item.setAttribute("aria-current", "page");
        else item.removeAttribute("aria-current");
      }
      const context: PageContext = {
        api: this.api,
        content: fresh,
        notify: (message) => this.notify(message),
        navigate: (page) => this.navigate(page),
      };
      if (page.startsWith("extension/")) {
        fresh.append(el("p", "muted", "Loading extension…"));
        void this.extensions.mount(page.slice(10), fresh).catch((error) => {
          if (fresh.isConnected) fresh.replaceChildren(errorPanel(error));
        });
        setLocalizedPageTitle("Extension");
      } else {
        const corePage = page as Page;
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
