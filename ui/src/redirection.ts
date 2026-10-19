import { invoke } from "@tauri-apps/api/core";
import type { CommandError } from "./bindings/console";
import { reason } from "./Session.svelte";

/** The last session of a kind closing: the next opens once it's gone, AMT takes one at a time. */
const closing: Record<string, Promise<unknown>> = {};

/**
 * Keeps a redirection session to `host` open, for an `$effect` to return what this returns, which closes it. Once open
 * the session is `use`d, as long as `open` says so. Until then `status` says what we wait for, for the footer, and
 * nothing once it's open. When it fails we open it again a little later, longer after each failure in a row, up to
 * half a minute, so a device that's busy isn't kept busier. Where the device wants consent we ask for it with
 * `consent`, and open it again at once if given, or stop if not: `stopped` says why.
 */
export function stayOpen(
  kind: "kvm" | "sol",
  host: string,
  use: (open: () => boolean) => Promise<unknown>,
  status: (text: string) => void,
  consent: () => Promise<boolean>,
  stopped: (reason: string) => void,
) {
  const what = kind === "kvm" ? "the screen" : "the serial console";
  let open = true;
  (async () => {
    status(`Opening ${what}…`);
    for (let failures = 0; open; failures++) {
      try {
        await closing[kind];
        if (!open) return;
        await invoke(`${kind}_open`, { host });
        failures = 0;
        status("");
        await use(() => open);
      } catch (failure) {
        if (!open) return;
        if ((failure as CommandError).kind === "Consent") {
          if (await consent()) {
            failures = -1; // none in a row, once the loop counts this one
            continue;
          }
          status("");
          return stopped("Consent wasn't given.");
        }
        status(`Reconnecting ${what}, ${reason(failure)}`);
        await new Promise((resume) => setTimeout(resume, Math.min(3000 * 2 ** failures, 30_000)));
      }
    }
  })();
  return () => {
    open = false;
    status("");
    closing[kind] = invoke(`${kind}_close`, { host });
  };
}
