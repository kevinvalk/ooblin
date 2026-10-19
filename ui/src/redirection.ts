import { invoke } from "@tauri-apps/api/core";
import type { CommandError } from "./bindings/console";

/** The last session of a kind closing: the next opens once it's gone, AMT takes one at a time. */
const closing: Record<string, Promise<unknown>> = {};

/**
 * Keeps a redirection session to `host` open, for an `$effect` to return what this returns, which closes it. Once open
 * the session is `use`d, as long as `open` says so. When it fails, `failed` says why and we open it again a little
 * later, longer after each failure in a row, up to half a minute, so a device that's busy isn't kept busier.
 */
export function stayOpen(
  kind: "kvm" | "sol",
  host: string,
  use: (open: () => boolean) => Promise<unknown>,
  failed: (reason: string) => void,
) {
  let open = true;
  (async () => {
    for (let failures = 0; open; failures++) {
      try {
        await closing[kind];
        if (!open) return;
        await invoke(`${kind}_open`, { host });
        failures = 0;
        await use(() => open);
      } catch (failure) {
        if (!open) return;
        failed((failure as CommandError).message ?? String(failure));
        await new Promise((resume) => setTimeout(resume, Math.min(3000 * 2 ** failures, 30_000)));
      }
    }
  })();
  return () => {
    open = false;
    closing[kind] = invoke(`${kind}_close`, { host });
  };
}
