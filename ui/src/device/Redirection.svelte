<!-- The black box a device's screen or serial console shows in, which keeps its session (`kind`) open while it's here,
     however often it drops: AMT drops the screen while the device boots, say. Once open it's `read`, as long as `open`
     says so. Until then `status` says what we wait for, for the footer, and nothing once it's open. When it fails we
     open it again a little later, longer after each failure in a row, up to half a minute, so a device that's busy isn't
     kept busier. Where the device wants consent we ask for it with `consent`, and open it again at once if given, or
     stop if not, until connected again by hand. -->
<script module lang="ts">
  /** The last session of a kind to a host closing: the next opens once it's gone, AMT takes one at a time. */
  const closing: Record<string, Promise<unknown>> = {};
</script>

<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import type { Snippet } from "svelte";
  import type { CommandError } from "../bindings/ooblin";
  import { reason } from "./Device.svelte";

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
    read: (open: () => boolean) => Promise<unknown>;
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
        try {
          await closing[kind + target];
          if (!open) return;
          await invoke(`${kind}_open`, { host: target });
          failures = 0;
          status("");
          await read(() => open);
        } catch (failure) {
          if (!open) return;
          if ((failure as CommandError).kind === "Consent") {
            if (await consent()) {
              failures = -1; // none in a row, once the loop counts this one
              continue;
            }
            status("");
            stopped = "Consent wasn't given.";
            return;
          }
          status(`Reconnecting ${what}, ${reason(failure)}`);
          await new Promise((resume) => setTimeout(resume, Math.min(3000 * 2 ** failures, 30_000)));
        }
      }
    })();
    return () => {
      open = false;
      status("");
      closing[kind + target] = invoke(`${kind}_close`, { host: target });
    };
  });
</script>

<div class="relative grid min-h-0 place-items-center overflow-auto bg-black">
  {@render children()}
  {#if stopped}
    <div class="absolute inset-0 grid place-content-center justify-items-center gap-3 bg-black/80 text-white/80">
      <p>{stopped}</p>
      <button type="button" class="btn btn-primary" onclick={() => (stopped = "")}>Connect</button>
    </div>
  {/if}
</div>
