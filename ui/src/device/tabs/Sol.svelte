<!-- The device's serial console (Serial over LAN): what it writes, and what's typed here. 80 by 25, the size serial
     consoles assume, as the device can't know ours. Like the screen it stays open while hidden, and comes back when it
     goes away, the footer saying so meanwhile. Without consent it stops, until connected again. -->
<script lang="ts">
  import "@xterm/xterm/css/xterm.css";
  import { invoke } from "@tauri-apps/api/core";
  import { Terminal } from "@xterm/xterm";
  import Redirection from "../Redirection.svelte";

  let {
    id: panel,
    host,
    hidden,
    consent,
    status,
  }: {
    id: string;
    host: string;
    hidden: boolean;
    consent: () => Promise<boolean>;
    status: (text: string) => void;
  } = $props();

  const terminal = new Terminal({
    cols: 80,
    rows: 25,
    fontFamily: getComputedStyle(document.documentElement).getPropertyValue("--font-mono"),
    fontSize: 14,
    theme: { background: "#000000" },
  });
  /** The open session's id, which what's typed goes to: typing for one since closed goes nowhere. */
  let id: number | undefined;

  // What's typed goes out in the order it was typed. A console that's gone says so in its reads. Backspace is ^H, as
  // MeshCommander sends it: what the BIOS takes, and shells do too.
  let typing: Promise<unknown> = Promise.resolve();
  terminal.onData((data) => {
    const to = id;
    if (to === undefined) return;
    data = data.replaceAll("\x7f", "\b");
    typing = typing.then(() => invoke("sol_write", { host, id: to, data }).catch(() => {}));
  });

  /** What the device writes, while it's open. */
  async function read(opened: number, open: () => boolean) {
    id = opened;
    if (!hidden) terminal.focus();
    while (open()) terminal.write(new Uint8Array(await invoke<ArrayBuffer>("sol_read", { host, id: opened })));
  }
</script>

<div id={panel} class="grid min-h-0 px-8 pt-6 pb-8" role="tabpanel" aria-label="Serial console" {hidden}>
  <Redirection kind="sol" {host} {consent} {status} {read}>
    <div
      {@attach (element) => {
        terminal.open(element);
        return () => terminal.dispose();
      }}
    ></div>
  </Redirection>
</div>
