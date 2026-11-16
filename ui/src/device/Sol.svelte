<script lang="ts">
  import "@xterm/xterm/css/xterm.css";
  import { invoke } from "@tauri-apps/api/core";
  import { Terminal } from "@xterm/xterm";
  import Redirection from "./Redirection.svelte";

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

  // We use 80 by 25, the size serial consoles assume, as the device cannot know ours.
  const terminal = new Terminal({
    cols: 80,
    rows: 25,
    fontFamily: getComputedStyle(document.documentElement).getPropertyValue("--font-mono"),
    fontSize: 14,
    theme: { background: "#000000" },
  });

  /** The id of the open session, which what is typed goes to: typing for one since closed goes nowhere. */
  let id: number | undefined;

  // What is typed goes out in the order it was typed, and a console that is gone says so in its reads. Backspace is ^H
  // as MeshCommander sends it, which is what the BIOS takes and shells do too.
  let typing: Promise<unknown> = Promise.resolve();
  terminal.onData((text) => {
    const openId = id;
    if (openId === undefined) return;

    text = text.replaceAll("\x7f", "\b");
    typing = typing.then(() => invoke("sol_write", { host, id: openId, text }).catch(() => {}));
  });

  async function read(opened: number, isOpen: () => boolean) {
    id = opened;
    if (!hidden) terminal.focus();

    while (isOpen()) terminal.write(new Uint8Array(await invoke<ArrayBuffer>("sol_read", { host, id: opened })));
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
