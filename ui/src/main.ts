import * as amt from "./amt";

const $ = <T extends HTMLElement = HTMLElement>(selector: string, root: ParentNode = document) =>
  root.querySelector<T>(selector)!;

const form = $<HTMLFormElement>("#connect");
const fields = form.elements as unknown as Record<"host" | "user" | "password" | "remember", HTMLInputElement>;
const session = $("#session");
const header = $("main > header");

let devices: amt.Device[] = [];
/** Open sessions by host, and the host on screen (none: the connect form). */
const sessions = new Map<string, amt.Info>();
let current: string | null = null;

// The sidebar: every device we logged in to, the open ones marked.

async function listDevices() {
  devices = await amt.devices().catch((error) => (say(form, message(error)), []));
  $("#devices").replaceChildren(
    ...devices.map((device) => {
      const item = clone("device").firstElementChild as HTMLElement;
      fill(item, { host: device.host });
      item.classList.toggle("connected", sessions.has(device.host));
      if (device.host === current) $(".host", item).setAttribute("aria-current", "true");
      $(".host", item).addEventListener("click", () => select(device));
      $(".disconnect", item).addEventListener("click", () => disconnect(device.host));
      $(".forget", item).addEventListener("click", () => amt.forget(device.host).then(listDevices));
      return item;
    }),
  );
}

/** Open sessions show, remembered devices log in, the rest ask for their password. */
function select(device: amt.Device) {
  if (sessions.has(device.host)) showSession(device.host);
  else if (device.remembered) attempt(device, () => amt.reconnect(device.host));
  else {
    showForm(device);
    fields.password.focus();
  }
}

async function disconnect(host: string) {
  await amt.disconnect(host);
  sessions.delete(host);
  if (host === current) showForm();
  else listDevices();
}

$("[data-action=new]").addEventListener("click", () => {
  showForm();
  fields.host.focus();
});

$("[data-action=sidebar]").addEventListener("click", toggleSidebar);
addEventListener("keydown", (event) => {
  if ((event.metaKey || event.ctrlKey) && event.key === "b") {
    event.preventDefault();
    toggleSidebar();
  }
});

function toggleSidebar() {
  document.body.classList.toggle("collapsed");
}

// The connect form.

form.addEventListener("submit", (event) => {
  event.preventDefault();
  const target = { host: fields.host.value, user: fields.user.value, password: fields.password.value };
  attempt(target, () => amt.connect(target, fields.remember.checked));
});

/** Shows what we're connecting to in the form, so a failure leaves everything in place to fix and retry. */
async function attempt({ host, user }: { host: string; user: string }, connect: () => Promise<amt.Info>) {
  showForm({ host, user });
  form.inert = true;
  $("button[type=submit]", form).textContent = "Connecting…";
  try {
    sessions.set(host, await connect());
    showSession(host);
  } catch (error) {
    say(form, explain(error, host, () => attempt({ host, user }, connect)));
  } finally {
    form.inert = false;
    $("button[type=submit]", form).textContent = "Connect";
  }
}

function showForm(device?: { host: string; user: string }) {
  form.reset();
  if (device) {
    fields.host.value = device.host;
    fields.user.value = device.user;
  }
  say(form);
  show(null);
  session.hidden = true;
  form.hidden = false;
}

/** Pin mismatches get both fingerprints and a way to trust the new one, then `retry`. */
function explain(failure: unknown, host: string, retry: () => void): string | Node {
  const error = failure as amt.CommandError;
  if (error?.kind !== "pinMismatch") return message(failure);

  const mismatch = clone("pin-mismatch");
  fill(mismatch, { expected: twoLines(error.expected), actual: twoLines(error.actual) });
  $("[data-action=trust]", mismatch).addEventListener("click", () => amt.trust(host, error.actual).then(retry));
  return mismatch;
}

// A session: its tabs, power and hardware.

function showSession(host: string) {
  const info = sessions.get(host)!;
  const modes = { admin: "Admin control mode", client: "Client control mode", unknown: "Unknown control mode" };
  show(host, {
    name: info.domain ? `${info.hostname}.${info.domain}` : info.hostname,
    version: `AMT ${info.version}` + (info.build ? ` · build ${info.build}` : ""),
    mode: modes[info.controlMode],
  });
  fill(session, {
    realm: info.digestRealm,
    fingerprint: twoLines(info.fingerprint),
    trust: info.trust === "pinned" ? "Matches the pinned certificate" : "First connection, pinned just now",
  });
  say(session);
  form.hidden = true;
  session.hidden = false;
  showTab("overview");
  refreshPower(host);
}

/** Puts `host` (or the form) on screen: the sidebar marks it, the header and the window title name it. */
function show(host: string | null, title = { name: "", version: "", mode: "" }) {
  current = host;
  fill(header, title);
  amt.title(host ? `${title.name} – AMT Console` : "AMT Console");
  listDevices();
}

for (const tab of session.querySelectorAll<HTMLButtonElement>("[data-tab]")) {
  tab.addEventListener("click", () => showTab(tab.dataset.tab!));
}

function showTab(name: string) {
  for (const tab of session.querySelectorAll<HTMLElement>("[data-tab]")) {
    tab.setAttribute("aria-selected", String(tab.dataset.tab === name));
  }
  for (const panel of session.querySelectorAll<HTMLElement>("[data-panel]")) panel.hidden = panel.dataset.panel !== name;
  if (name === "hardware") loadInventory(current!);
}

for (const button of session.querySelectorAll<HTMLButtonElement>("[data-power]")) {
  button.addEventListener("click", async () => {
    const host = current!;
    const verb = button.textContent!;
    const action = button.dataset.power as amt.PowerAction;
    if (action !== "on" && !(await confirm(`${verb} ${$("[data-field=name]", header).textContent}?`, verb))) return;
    try {
      await amt.power(host, action);
      await refreshPower(host);
    } catch (error) {
      say(session, message(error));
    }
  });
}

/** Results for a session we've since left are dropped. */
async function refreshPower(host: string) {
  const labels = { on: "On", sleep: "Asleep", hibernate: "Hibernating", off: "Off", unknown: "Unknown" };
  const state = $("[data-field=power]", session);
  state.textContent = "…";
  delete state.dataset.state;
  try {
    const power = await amt.powerState(host);
    if (host !== current) return;
    state.textContent = labels[power];
    state.dataset.state = power;
  } catch (error) {
    if (host === current) say(session, message(error));
  }
}

async function loadInventory(host: string) {
  $("#inventory").replaceChildren();
  try {
    const inventory = await amt.inventory(host);
    if (host !== current) return;
    const rows: [string, string][] = [
      ["System", `${inventory.manufacturer} ${inventory.model}`],
      ["Serial", inventory.serial],
      ["Board", inventory.board],
      ["BIOS", `${inventory.bios} (${inventory.biosDate})`],
      ["Processor", inventory.processor],
      ...inventory.memory.map(({ bank, bytes, mhz, part }): [string, string] => [
        "Memory",
        `${bank}: ${bytes / 2 ** 30} GB, ${mhz} MHz, ${part}`,
      ]),
    ];
    // Repeated terms (several DIMMs) only get their label once.
    $("#inventory").replaceChildren(
      ...rows.flatMap(([term, value], i) => [
        ...(rows[i - 1]?.[0] === term ? [] : [element("dt", term)]),
        element("dd", value),
      ]),
    );
  } catch (error) {
    if (host === current) say(session, message(error));
  }
}

function confirm(question: string, verb: string): Promise<boolean> {
  const dialog = $<HTMLDialogElement>("#confirm");
  fill(dialog, { question, verb });
  dialog.returnValue = "";
  dialog.showModal();
  return new Promise((resolve) =>
    dialog.addEventListener("close", () => resolve(dialog.returnValue === "confirm"), { once: true }),
  );
}

// Helpers.

/** Fills `data-field` slots as text, never as markup. */
function fill(root: ParentNode, fields: Record<string, string>) {
  for (const [name, value] of Object.entries(fields)) $(`[data-field=${name}]`, root).textContent = value;
}

function clone(template: string): DocumentFragment {
  return $<HTMLTemplateElement>(`template#${template}`).content.cloneNode(true) as DocumentFragment;
}

function element(tag: string, text: string): HTMLElement {
  return Object.assign(document.createElement(tag), { textContent: text });
}

/** Shows `content` in the `<output>` of a view, or clears it. */
function say(view: HTMLElement, content?: string | Node) {
  $("output", view).replaceChildren(...(content ? [content] : []));
}

function message(error: unknown): string {
  return (error as amt.CommandError)?.message ?? String(error);
}

/** 32 bytes as two rows of 16, so fingerprints can be compared at a glance. */
function twoLines(fingerprint: string): string {
  const bytes = fingerprint.split(":");
  return `${bytes.slice(0, 16).join(":")}\n${bytes.slice(16).join(":")}`;
}

// Started with a host on the command line? Open it as if it was picked from the sidebar.
amt.launch().then((device) => (device ? select(device) : showForm()));
