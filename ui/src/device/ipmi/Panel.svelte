<!-- The server's front panel, in the header: its identify light (UID) to switch, lit like the real one while it's on,
     and the last POST code its BIOS wrote, what the BMC doesn't say left out. Read every 5 seconds or so while the
     device is shown, and at once after a switch. -->
<script lang="ts">
  import { Lightbulb } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { Panel } from "../../bindings/core";
  import { reason } from "../Device.svelte";

  let { host, hidden, say }: { host: string; hidden: boolean; say: (text: string, failed?: boolean) => void } = $props();

  let panel = $state<Panel>();
  let switching = $state(false);

  async function read() {
    panel = await invoke<Panel>("panel", { host }).catch(() => panel);
  }

  // Each read 5 seconds after the one before landed, so a slow BMC isn't asked again before it answered.
  $effect(() => {
    if (hidden) return;
    let shown = true;
    (async () => {
      for (; shown; await new Promise((resume) => setTimeout(resume, 5000))) await read();
    })();
    return () => (shown = false);
  });

  async function identify(on: boolean) {
    switching = true;
    try {
      await invoke("identify", { host, on });
      say(on ? "Identify light on." : "Identify light off.");
      await read();
    } catch (error) {
      say(reason(error), true);
    } finally {
      switching = false;
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
{#if panel?.identify != null}
  <button
    type="button"
    class={["btn btn-secondary", panel.identify && "border-accent/60 text-accent"]}
    aria-pressed={panel.identify}
    disabled={switching}
    title="Lights the server's blue identify light (UID), to find it in the rack, until switched off."
    onclick={() => identify(!panel!.identify)}
  >
    <Lightbulb class={["size-4", panel.identify && "fill-current"]} /> UID
  </button>
{/if}
