import * as amt from "./amt";

const form = document.querySelector<HTMLFormElement>("#connect")!;
const output = form.querySelector("output")!;
const device = document.querySelector<HTMLElement>("#device")!;

form.addEventListener("submit", (event) => {
  event.preventDefault();
  connect(Object.fromEntries(new FormData(form)) as unknown as amt.Target);
});

device.querySelector("[data-action=disconnect]")!.addEventListener("click", () => {
  device.hidden = true;
  form.hidden = false;
});

async function connect(target: amt.Target) {
  form.inert = true;
  output.replaceChildren("Connecting…");
  try {
    const info = await amt.connect(target);
    output.replaceChildren();
    fill(device, {
      name: info.domain ? `${info.hostname}.${info.domain}` : info.hostname,
      version: `AMT ${info.version}` + (info.build ? ` · build ${info.build}` : ""),
      mode: { admin: "Admin control mode", client: "Client control mode", unknown: "Unknown control mode" }[info.controlMode],
      realm: info.digestRealm,
      fingerprint: twoLines(info.fingerprint),
      trust: info.trust === "pinned" ? "Matches the pinned certificate" : "First connection, pinned just now",
    });
    form.hidden = true;
    device.hidden = false;
  } catch (error) {
    output.replaceChildren(explain(error, target));
  } finally {
    form.inert = false;
  }
}

function explain(failure: unknown, target: amt.Target): Node {
  const error = failure as amt.CommandError;
  if (error?.kind !== "pinMismatch") return document.createTextNode(error?.message ?? String(failure));

  const mismatch = document.querySelector<HTMLTemplateElement>("#pin-mismatch")!.content.cloneNode(true) as DocumentFragment;
  fill(mismatch, { expected: twoLines(error.expected), actual: twoLines(error.actual) });
  mismatch.querySelector("[data-action=trust]")!.addEventListener("click", async () => {
    await amt.trust(target.host, error.actual);
    connect(target);
  });
  return mismatch;
}

/** Fills `data-field` slots as text, never as markup. */
function fill(root: ParentNode, fields: Record<string, string>) {
  for (const [name, value] of Object.entries(fields)) root.querySelector(`[data-field=${name}]`)!.textContent = value;
}

/** 32 bytes as two rows of 16, so fingerprints can be compared at a glance. */
function twoLines(fingerprint: string): string {
  const bytes = fingerprint.split(":");
  return `${bytes.slice(0, 16).join(":")}\n${bytes.slice(16).join(":")}`;
}
