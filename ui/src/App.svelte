<!-- The window: devices on the left, the connect form or a device on the right, a status line at the bottom. -->
<script lang="ts">
  import { PanelLeft, ScrollText } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import * as log from "@tauri-apps/plugin-log";
  import type { CommandError, Connected, Device as Saved } from "./bindings/ooblin";
  import Connect, { type Login } from "./Connect.svelte";
  import Device, { reason, type Connection } from "./device/Device.svelte";
  import Sidebar from "./Sidebar.svelte";

  /** Every device we logged in to, the open ones by host, and the one on screen (none: the connect form). */
  let devices: Saved[] = $state.raw([]);
  let connections: Record<string, Connection> = $state({});
  let current: string | null = $state(null);
  /** The connect form, for `device` if given: a new `key` is a fresh one. `error` is why the last attempt failed. */
  let form = $state({
    key: 0,
    device: undefined as Saved | undefined,
    error: undefined as CommandError | undefined,
  });
  /** The device we're connecting to: it spins, and a second click on it does nothing. */
  let connecting: string | null = $state(null);
  let collapsed = $state(false);
  let message = $state({ text: "", failed: false });
  let hide = 0;

  /** Says `text` in the footer for as long as it takes to read: five seconds, more for more words, fifteen at most.
   * What `failed` stands out, and goes in the log. */
  function say(text: string, failed = false) {
    message = { text, failed };
    if (failed) log.warn(text).catch(() => {});
    clearTimeout(hide);
    hide = setTimeout(() => (message.text = ""), Math.min(5000 + 50 * text.length, 15_000));
  }

  async function listDevices() {
    devices = await invoke<Saved[]>("devices").catch((error) => (say(String(error), true), []));
  }

  /** Open devices show, remembered devices log in, the rest ask for their password. */
  function select(device: Saved) {
    if (connections[device.host]) return show(device.host);
    if (connecting === device.host) return;
    showForm(device);
    if (device.remembered)
      connect({ host: device.host, user: device.user, password: null, remember: false, flavor: null });
  }

  function showForm(device?: Saved, error?: CommandError) {
    form = { key: form.key + 1, device, error };
    current = null;
    say("");
    listDevices();
  }

  /** Without a `password` with the one we have, see the `connect` command. The form may have made way for another
   * one meanwhile, or a device: then it's in the sidebar, or the footer says why not. */
  async function connect(login: Omit<Login, "password"> & { password: string | null }) {
    const host = login.host;
    const key = form.key;
    const shown = () => !current && form.key === key;
    connecting = host;
    form.error = undefined;
    try {
      const { info, unremembered } = await invoke<Connected>("connect", login);
      connections[host] = { host, info };
      if (shown()) show(host);
      else listDevices();
      if (unremembered) say(`Connected, but the password couldn't be remembered: ${unremembered}`, true);
    } catch (error) {
      if (shown()) form.error = explain(error, `Couldn't reach ${host}`);
      else say(`Couldn't connect to ${host}: ${reason(error)}`, true);
    } finally {
      if (connecting === host) connecting = null;
    }
  }

  /** Our commands fail with a `CommandError`, Tauri itself with a string. A device that didn't answer gets named. */
  function explain(error: unknown, unreachable: string): CommandError {
    const failure = typeof error === "string" ? { kind: "Failed" as const, message: error } : (error as CommandError);
    return failure.kind === "Unreachable" ? { ...failure, message: `${unreachable}: ${failure.message}` } : failure;
  }

  function show(host: string) {
    current = host;
    say("");
    listDevices();
  }

  /** Gone at once, while Rust closes what was open on it. */
  function disconnect(host: string) {
    delete connections[host];
    if (host === current) showForm();
    invoke("disconnect", { host });
  }

  async function forget(host: string) {
    await invoke("forget", { host }).catch((error) => say(reason(error), true));
    listDevices();
  }

  /** The device stopped answering for good: we let go of it and say why, Connect tries again. */
  function lost(host: string, error: CommandError) {
    delete connections[host];
    invoke("disconnect", { host });
    const failure = explain(error, `Lost the connection to ${host}`);
    if (host === current) showForm(devices.find((device) => device.host === host), failure);
    else say(failure.message, true);
  }

  // The window is titled after the device on screen.
  $effect(() => {
    invoke("title", {
      title: current ? `${connections[current].info.name || current} – Ooblin` : "Ooblin",
    });
  });

  function keydown(event: KeyboardEvent) {
    if ((event.metaKey || event.ctrlKey) && event.key === "b") {
      event.preventDefault();
      collapsed = !collapsed;
    }
  }

  /** What goes wrong unnoticed, an error thrown or a promise rejected, goes in the log. */
  function unnoticed(event: Event) {
    const error = event instanceof ErrorEvent ? event.error : (event as PromiseRejectionEvent).reason;
    log.error(error instanceof Error ? (error.stack ?? error.message) : reason(error)).catch(() => {});
  }

  /** Saves the log where asked, see `save_log`. */
  async function saveLog() {
    const path = await invoke<string | null>("pick_save", { name: "Ooblin.log" });
    if (!path) return;
    try {
      await invoke("save_log", { path });
      say(`Log saved to ${path}.`);
    } catch (error) {
      say(reason(error), true);
    }
  }

  // Started with a host on the command line, or started again with one? Open it as if it was picked from the sidebar.
  invoke<Saved | null>("launch").then((device) => (device ? select(device) : showForm()));
  listen("launch", () => invoke<Saved | null>("launch").then((device) => device && select(device)));
</script>

<svelte:window onkeydown={keydown} onerror={unnoticed} onunhandledrejection={unnoticed} />

<div class={["grid h-screen grid-rows-[1fr_auto]", collapsed ? "grid-cols-[0_1fr]" : "grid-cols-[16rem_1fr]"]}>
  <Sidebar
    {devices}
    {connections}
    {current}
    {connecting}
    {collapsed}
    {say}
    onselect={select}
    ondisconnect={disconnect}
    onforget={forget}
    onnew={() => showForm()}
  />

  <div class="grid min-w-0 overflow-hidden">
    {#each Object.keys(connections) as host (host)}
      <Device bind:connection={connections[host]} hidden={host !== current} {say} lost={(error) => lost(host, error)} />
    {/each}
    {#if !current}
      {#key form.key}
        <Connect
          device={form.device}
          connecting={connecting !== null}
          error={form.error}
          onsubmit={(login) => connect({ ...login, password: login.password || null })}
          ontrust={(host, fingerprint) => invoke("trust", { host, fingerprint })}
        />
      {/key}
    {/if}
  </div>

  <footer class="col-span-full flex h-8 items-center gap-2 border-t border-line bg-pane px-1.5">
    <button
      type="button"
      class="btn-icon size-7"
      title="Toggle the sidebar (⌘B)"
      onclick={() => (collapsed = !collapsed)}
    >
      <PanelLeft class="size-4" />
    </button>
    <!-- What's true now outlasts what just happened. -->
    {#if current && connections[current].silent}
      <output class="truncate text-muted">{current} isn't answering, trying again…</output>
    {:else}
      <!-- Selectable, and kept while the pointer is on it, to copy. -->
      <output
        class={["truncate select-text", message.failed ? "text-danger" : "text-muted"]}
        onmouseenter={() => clearTimeout(hide)}
        onmouseleave={() => message.text && say(message.text, message.failed)}
      >
        {message.text}
      </output>
    {/if}
    <!-- Then what the device on screen says, on the right. -->
    {#if current}{@render connections[current].footer?.()}{/if}
    <button
      type="button"
      class={["btn-icon size-7", !current && "ml-auto"]}
      title="Save the log: what Ooblin did and what went wrong, with the devices' addresses and names, never a password"
      onclick={saveLog}
    >
      <ScrollText class="size-4" />
    </button>
  </footer>
</div>
