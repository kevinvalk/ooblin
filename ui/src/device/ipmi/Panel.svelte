<script lang="ts">
  import { Lightbulb, LoaderCircle } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { Panel } from "../../bindings/core";
  import { reason } from "../../connection";

  let { host, hidden, say }: { host: string; hidden: boolean; say: (text: string, isFailed?: boolean) => void } = $props();

  let panel = $state<Panel>();
  let isSwitching = $state(false);

  async function read() {
    // We read every few seconds, so a read that failed keeps what we had instead of nagging the user each time.
    panel = await invoke<Panel>("panel", { host }).catch(() => panel);
  }

  // We read while the device is shown, each time 5 seconds after the read before landed, so that a slow BMC is not
  // asked again before it answered.
  $effect(() => {
    if (hidden) return;

    let isShown = true;
    (async () => {
      for (; isShown; await new Promise((resume) => setTimeout(resume, 5000))) await read();
    })();

    return () => (isShown = false);
  });

  async function identify(isOn: boolean) {
    isSwitching = true;
    try {
      await invoke("identify", { host, isOn });
      say(isOn ? "Identify light on." : "Identify light off.");
      await read();
    } catch (error) {
      say(reason(error), true);
    } finally {
      isSwitching = false;
    }
  }
</script>

{#if panel?.post_code != null}
  <span
    class="rounded-md border border-line px-2 py-1 font-mono text-xs text-muted tabular-nums"
    title="The last POST code the BIOS wrote: while it starts, how far it got."
  >
    POST {panel.post_code.toString(16).toUpperCase().padStart(2, "0")}
  </span>
{/if}
{#if panel?.is_identifying != null}
  <button
    type="button"
    class={["btn btn-secondary", panel.is_identifying && "border-accent/60 text-accent"]}
    aria-pressed={panel.is_identifying}
    disabled={isSwitching}
    title="Lights the server's blue identify light (UID), to find it in the rack, until switched off."
    onclick={() => identify(!panel!.is_identifying)}
  >
    {#if isSwitching}
      <LoaderCircle class="size-4 animate-spin" />
    {:else}
      <Lightbulb class={["size-4", panel.is_identifying && "fill-current"]} />
    {/if} UID
  </button>
{/if}
