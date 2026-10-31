namespace Amaterasu {
  export interface SessionExtensionAction {
    id: string;
    label: string;
    description: string;
    execution: string;
    arguments: Record<string, ExtensionFieldDefinition>;
    allowed_targets: string[];
  }
  export interface ExtensionActionReceipt {
    status: string;
    operation_id: string;
    reason?: string | null;
    result?: unknown;
  }

  export class ExtensionActionClient {
    private readonly openId = Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) => byte.toString(16).padStart(2, "0")).join("");
    private epoch: string | null = null;
    private submitted = false;
    constructor(private readonly api: Api) {}
    get wasSubmitted(): boolean { return this.submitted; }

    async invoke(actionId: string, args: Record<string, unknown>): Promise<ExtensionActionReceipt> {
      if (this.submitted) throw new Error("This action may already have been submitted. Check its status before starting another.");
      const snapshot = await this.api.post<{ host_epoch: string }>("extension-actions-target/snapshot");
      if (typeof snapshot.host_epoch !== "string" || !snapshot.host_epoch) throw new Error("Invalid host snapshot.");
      if (!this.epoch) {
        const opened = await this.api.post<{ epoch: string }>("extension-actions/open_epoch", { operation_id: this.openId });
        if (typeof opened.epoch !== "string" || !opened.epoch) throw new Error("Invalid action epoch.");
        this.epoch = opened.epoch;
      }
      const epoch = this.epoch;
      this.submitted = true; // A timeout does not prove the request had no effect.
      return this.api.post<ExtensionActionReceipt>("extension-actions/invoke", {
        epoch, sequence: 1, operation_id: `${epoch}:1`, action_id: actionId,
        target: { kind: "host", host_epoch: snapshot.host_epoch }, arguments: args, expected_settings_revision: null,
      });
    }

    async retireRejected(receipt: ExtensionActionReceipt): Promise<void> {
      if (!this.epoch || receipt.status !== "rejected" || receipt.operation_id !== `${this.epoch}:1`)
        throw new Error("Only a confirmed rejected receipt can be retired.");
      await this.api.post("extension-actions/close_epoch", { epoch: this.epoch });
      await this.api.post("extension-actions/retire_epoch", { epoch: this.epoch });
    }

    async status(): Promise<ExtensionActionReceipt> {
      if (!this.epoch || !this.submitted) throw new Error("No action has been submitted.");
      return this.api.post<ExtensionActionReceipt>("extension-actions/status", { epoch: this.epoch, sequence: 1 });
    }
  }

  export function renderExtensionAction(context: PageContext, action: SessionExtensionAction): HTMLElement {
    const form = el("form", "settings-form");
    const fields = new Map<string, ExtensionFieldEditor>();
    for (const [name, definition] of Object.entries(action.arguments)) {
      const editor = extensionFieldEditor(definition, structuredClone(definition.default));
      fields.set(name, editor);
      form.append(el("label", "field", el("span", "field-label", name), editor.node));
    }
    const status = el("div", "muted");
    status.setAttribute("aria-live", "polite");
    const submit = el("button", "button primary", action.label); submit.type = "submit";
    submit.disabled = !context.api.allows("configuration.write");
    const check = button("Check action status", async () => {
      try { show(await client.status()); } catch (error) { status.textContent = messageOf(error); }
    });
    check.disabled = true;
    const client = new ExtensionActionClient(context.api);
    function show(receipt: ExtensionActionReceipt): void {
      status.textContent = `Action ${receipt.status}${receipt.reason ? `: ${receipt.reason}` : ""}`;
    }
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      void (async () => {
        submit.disabled = true;
        try {
          const args = Object.fromEntries([...fields].map(([name, editor]) => [name, editor.value()]));
          const result = await client.invoke(action.id, args);
          check.disabled = false;
          show(result);
          if (result.status === "rejected") {
            try { await client.retireRejected(result); check.disabled = true; } catch { /* Keep status available if cleanup failed. */ }
          }
        } catch (error) {
          status.textContent = messageOf(error);
          check.disabled = !client.wasSubmitted;
          submit.disabled = client.wasSubmitted || !context.api.allows("configuration.write");
          // A lost reply may conceal a committed effect. The same client never resubmits it.
        }
      })();
    });
    form.append(el("p", "muted", action.description), submit, check, status);
    return form;
  }
}
