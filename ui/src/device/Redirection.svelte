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
    read: (id: number, isOpen: () => boolean) => Promise<unknown>;
    children: Snippet;
  } = $props();

  /** Why it stopped, until connected again. */
  let stopped = $state("");

  // We keep the session (`kind`) of the screen or serial console open while we are here, however often it drops: AMT
  // drops the screen while the device boots, say. Once open it is `read` by the id Rust gave it for as long as `isOpen`
  // says so and then closed, as Rust closes the one before as the next opens. Until it is open `status` says what we
  // wait for, for the footer.
  $effect(() => {
    if (stopped !== "") return;

    const [target, subject] = [host, kind === "kvm" ? "the screen" : "the serial console"];
    let isOpen = true;

    (async () => {
      status(`Opening ${subject}…`);

      for (let failures = 0; isOpen; failures++) {
        let id: number | undefined;
        try {
          id = await invoke<number>(`${kind}_open`, { host: target });
          if (!isOpen) return;

          failures = 0;
          status("");
          await read(id, () => isOpen);
        } catch (error) {
          if (!isOpen) return;

          // Where the device wants consent we ask for it, and open the session again at once if it is given.
          const { kind: errorKind } = error as CommandError;
          if (errorKind === "Consent" && (await consent())) {
            failures = -1; // None in a row, once the loop counts this one
            continue;
          }

          // Without consent we stop until connected again by hand. We stop too where the device ends the session on
          // purpose (a firmware update of a BMC, say), as its own viewer does.
          if (errorKind === "Consent" || errorKind === "Ended") {
            status("");
            stopped = errorKind === "Consent" ? "Consent wasn't given." : `Stopped, ${reason(error)}.`;
            return;
          }

          // We open it again a little later, longer after each failure in a row and up to half a minute, so that a
          // device that is busy is not kept busier.
          status(`Reconnecting ${subject}, ${reason(error)}`);
          log.warn(`Reconnecting ${subject} on ${target}: ${reason(error)}`).catch(() => {});
          await new Promise((resume) => setTimeout(resume, Math.min(3000 * 2 ** failures, 30_000)));
        } finally {
          if (id !== undefined) invoke(`${kind}_close`, { host: target, id });
        }
      }
    })();

    return () => {
      isOpen = false;
      status("");
    };
  });
</script>

<!-- One cell the size of the box, so what is in it can shrink to fit: an `auto` one grows to its content first. -->
<div class="relative grid min-h-0 grid-cols-1 grid-rows-1 place-items-center overflow-auto bg-black">
  {@render children()}
  {#if stopped !== ""}
    <div class="absolute inset-0 grid place-content-center justify-items-center gap-3 bg-black/80 text-white/80">
      <p>{stopped}</p>
      <button type="button" class="btn btn-primary" onclick={() => (stopped = "")}>Connect</button>
    </div>
  {/if}
</div>
