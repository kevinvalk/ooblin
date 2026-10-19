<!-- The device's serial console (Serial over LAN): what it writes, and what's typed here. 80 by 25, the size serial
     consoles assume, as the device can't know ours. Like the screen it stays open while hidden, and comes back when it
     goes away. -->
<script lang="ts">
  import "@xterm/xterm/css/xterm.css";
  import { LoaderCircle } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { Terminal } from "@xterm/xterm";
  import { stayOpen } from "../redirection";

  let { host, hidden }: { host: string; hidden: boolean } = $props();

  let element: HTMLDivElement;
  let opened = $state(false);
  /** Why the console went away, until it's back. */
  let error = $state("");

  const terminal = new Terminal({
    cols: 80,
    rows: 25,
    fontFamily: getComputedStyle(document.documentElement).getPropertyValue("--font-mono"),
    fontSize: 14,
    theme: { background: "#000000" },
  });
  // What's typed goes out in the order it was typed. A console that's gone says so in its reads.
  let typing: Promise<unknown> = Promise.resolve();
  terminal.onData((data) => (typing = typing.then(() => invoke("sol_write", { host, data }).catch(() => {}))));

  $effect(() => {
    terminal.open(element);
    return () => terminal.dispose();
  });

  $effect(() => {
    const target = host;
    opened = false;
    return stayOpen(
      "sol",
      target,
      async (open) => {
        opened = true;
        error = "";
        terminal.focus();
        while (open()) terminal.write(new Uint8Array(await invoke<ArrayBuffer>("sol_read", { host: target })));
      },
      (reason) => {
        opened = false;
        error = reason;
      },
    );
  });
</script>

<main class="grid min-h-0 grid-rows-[auto_1fr] px-8 pb-8" {hidden}>
  <div class="box-content flex h-7 items-center gap-2 pt-4 pb-3">
    {#if !opened}
      <LoaderCircle class="size-4 flex-none animate-spin text-muted" />
      <span class="truncate text-muted" title={error}>
        {error ? `Reconnecting, ${error}` : "Opening the serial console…"}
      </span>
    {/if}
  </div>
  <div class="grid min-h-0 place-items-center overflow-auto bg-black">
    <div bind:this={element}></div>
  </div>
</main>
