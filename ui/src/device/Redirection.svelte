<!-- The black box a device's screen or serial console shows in, which keeps its session (`kind`) open while it's here,
     however often it drops: AMT drops the screen while the device boots, say. Once open it's `read` by the id Rust gave
     it, as long as `open` says so, then closed: Rust closes the one before as the next opens. Until then `status` says
     what we wait for, for the footer, and nothing once it's open. When it fails we open it again a little later, longer
     after each failure in a row, up to half a minute, so a device that's busy isn't kept busier. Where the device wants
     consent we ask for it with `consent`, and open it again at once if given, or stop if not, until connected again by
     hand. We stop too where the device ends it on purpose (a BMC's firmware update, say), as its own viewer does. -->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import * as log from "@tauri-apps/plugin-log";
  import type { Snippet } from "svelte";
  import type { CommandError } from "../bindings/ooblin";
  import { reason } from "../connection";

  let {
    kind,
    host,
    consent,
    status,
    read,
    children,
  }: {
    kind: "kvm" | "sol";
    host: string;
    consent: () => Promise<boolean>;
    status: (text: string) => void;
    read: (id: number, open: () => boolean) => Promise<unknown>;
    children: Snippet;
  } = $props();

  /** Why it stopped, until connected again. */
  let stopped = $state("");

  $effect(() => {
    if (stopped) return;
    const [target, what] = [host, kind === "kvm" ? "the screen" : "the serial console"];
    let open = true;
    (async () => {
      status(`Opening ${what}…`);
      for (let failures = 0; open; failures++) {
        let id: number | undefined;
        try {
          id = await invoke<number>(`${kind}_open`, { host: target });
          if (!open) return;
          failures = 0;
          status("");
          await read(id, () => open);
        } catch (failure) {
          if (!open) return;
          const { kind: failed } = failure as CommandError;
          if (failed === "Consent" && (await consent())) {
            failures = -1; // none in a row, once the loop counts this one
            continue;
          }
          if (failed === "Consent" || failed === "Ended") {
            status("");
            stopped = failed === "Consent" ? "Consent wasn't given." : `Stopped, ${reason(failure)}.`;
            return;
          }
          status(`Reconnecting ${what}, ${reason(failure)}`);
          log.warn(`Reconnecting ${what} on ${target}: ${reason(failure)}`).catch(() => {});
          await new Promise((resume) => setTimeout(resume, Math.min(3000 * 2 ** failures, 30_000)));
        } finally {
          if (id !== undefined) invoke(`${kind}_close`, { host: target, id });
        }
      }
    })();
    return () => {
      open = false;
      status("");
    };
  });
</script>

<!-- One cell the size of the box, so what's in it can shrink to fit: an `auto` one grows to its content first. -->
<div class="relative grid min-h-0 grid-cols-1 grid-rows-1 place-items-center overflow-auto bg-black">
  {@render children()}
  {#if stopped}
    <div class="absolute inset-0 grid place-content-center justify-items-center gap-3 bg-black/80 text-white/80">
      <p>{stopped}</p>
      <button type="button" class="btn btn-primary" onclick={() => (stopped = "")}>Connect</button>
    </div>
  {/if}
</div>
