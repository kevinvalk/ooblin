<script lang="ts">
  import { CircleHelp, Download, PanelLeft } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import * as log from "@tauri-apps/plugin-log";
  import { SvelteSet } from "svelte/reactivity";
  import type { CommandError, Connected, Device as Saved } from "./bindings/ooblin";
  import Connect, { type Login } from "./Connect.svelte";
  import { reason, type Connection } from "./connection";
  import Device from "./device/Device.svelte";
  import Sidebar from "./Sidebar.svelte";

  let devices: Saved[] = $state.raw([]); // Every device we logged in to
  let connections: Record<string, Connection> = $state({}); // The open ones, by host
  let current: string | null = $state(null); // The host of the one on screen, null shows the connect form

  /** The connect form is for `device` if given, and a new `key` makes a fresh one. `error` is why the last attempt
   * failed, `isConnecting` whether an attempt of the form itself is underway. */
  let form = $state({
    key: 0,
    device: undefined as Saved | undefined,
    error: undefined as CommandError | undefined,
    isConnecting: false,
  });

  /** The devices we are connecting to: they spin, and a second click on one does nothing. */
  const connecting = new SvelteSet<string>();

  let isCollapsed = $state(false);
  let update: string | null = $state(null); // The version of a newer release, which the footer offers
  let message = $state({ text: "", isFailed: false });
  let hideTimeout = 0;

  /** Says `text` in the footer. A failure stands out and goes in the log too. */
  function say(text: string, isFailed = false) {
    message = { text, isFailed };
    if (isFailed) log.warn(text).catch(() => {});
    linger();
  }

  /** The footer keeps a message for as long as it takes to read: five seconds, more for more words, fifteen at most. */
  function linger() {
    clearTimeout(hideTimeout);
    hideTimeout = setTimeout(() => (message.text = ""), Math.min(5000 + 50 * message.text.length, 15_000));
  }

  async function listDevices() {
    devices = await invoke<Saved[]>("devices").catch((error) => (say(reason(error), true), []));
  }

  /** Open devices show, remembered devices log in, the rest ask for their password. */
  function select(device: Saved) {
    if (connections[device.host] !== undefined) return show(device.host);
    if (connecting.has(device.host)) return;

    showForm(device);
    if (device.is_remembered)
      connect({ host: device.host, user: device.user, password: "", isRemember: false, flavor: null });
  }

  function showForm(device?: Saved, error?: CommandError) {
    form = { key: form.key + 1, device, error, isConnecting: false };
    current = null;
    say("");
    listDevices();
  }

  /** An empty `password` is the one we have, see the `connect` command. The form may have made way for another one
   * meanwhile, or for a device: then the device is in the sidebar, or the footer says why not. */
  async function connect(login: Login) {
    const host = login.host;
    const key = form.key;
    const isShown = () => current === null && form.key === key;

    connecting.add(host);
    form.error = undefined;
    form.isConnecting = true;

    try {
      const password = login.password === "" ? null : login.password;
      const { info, remember_error } = await invoke<Connected>("connect", { ...login, password });
      connections[host] = { host, info };

      if (isShown()) show(host);
      else listDevices();

      if (remember_error !== null) say(`Connected, but the password couldn't be remembered: ${remember_error}`, true);
    } catch (error) {
      if (isShown()) form.error = explain(error, `Couldn't reach ${host}`);
      else say(`Couldn't connect to ${host}: ${reason(error)}`, true);
    } finally {
      connecting.delete(host);
      if (form.key === key) form.isConnecting = false;
    }
  }

  /** Pins the certificate the device presents now, then logs in again. */
  async function trust(login: Login, fingerprint: string) {
    try {
      await invoke("trust", { host: login.host, fingerprint });
      connect(login);
    } catch (error) {
      say(reason(error), true);
    }
  }

  /** Our commands fail with a `CommandError`, Tauri itself with a string. A device that did not answer gets named. */
  function explain(error: unknown, unreachable: string): CommandError {
    const failure = typeof error === "string" ? { kind: "Failed" as const, message: error } : (error as CommandError);
    return failure.kind === "Unreachable" ? { ...failure, message: `${unreachable}: ${failure.message}` } : failure;
  }

  function show(host: string) {
    current = host;
    say("");
    listDevices();
  }

  /** The device is gone at once, while Rust closes what was open on it. */
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

  $effect(() => {
    invoke("title", {
      title: current !== null ? `${connections[current].info.name || current} – Ooblin` : "Ooblin",
    });
  });

  /** What goes wrong unnoticed, an error thrown or a promise rejected, goes in the log. Some errors come without an
   * error (those of a ResizeObserver), only with a message. */
  function unnoticed(event: Event) {
    const error =
      event instanceof ErrorEvent ? (event.error ?? event.message) : (event as PromiseRejectionEvent).reason;
    log.error(error instanceof Error ? (error.stack ?? error.message) : reason(error)).catch(() => {});
  }

  async function saveLog() {
    const path = await invoke<string | null>("pick_save", { name: "Ooblin.log" });
    if (path === null) return;

    try {
      await invoke("save_log", { path });
      say(`Log saved to ${path}.`);
    } catch (error) {
      say(reason(error), true);
    }
  }

  /** Looks for a newer release. Only when `isAsked` do we say how that went, as at start being offline is no news. */
  async function checkUpdate(isAsked: boolean) {
    try {
      update = await invoke<string | null>("check_update");
      if (isAsked) say(update !== null ? `Ooblin ${update} is out.` : "Ooblin is up to date.");
    } catch (error) {
      if (isAsked) say(reason(error), true);
    }
  }
  checkUpdate(false);

  /** Rust starts the new release once it is installed, so we only hear back when that failed. */
  async function installUpdate() {
    say(`Installing Ooblin ${update}, it restarts when done…`);
    await invoke("update").catch((error) => say(reason(error), true));
  }

  // Started with a host on the command line, or started again with one? We open it as if it was picked from the
  // sidebar.
  invoke<Saved | null>("launch").then((device) => (device !== null ? select(device) : showForm()));
  listen("launch", () => invoke<Saved | null>("launch").then((device) => device !== null && select(device)));
</script>

<svelte:window
  onkeydown={(event) => {
    const isCommand = event.metaKey || event.ctrlKey;

    // Reloading would forget the open devices, which Rust keeps, so a Device reads its tab again instead.
    if (event.key === "F5" || (isCommand && event.key.toLowerCase() === "r")) event.preventDefault();

    if (isCommand && event.key.toLowerCase() === "b") {
      event.preventDefault();
      isCollapsed = !isCollapsed;
    }
  }}
  onerror={unnoticed}
  onunhandledrejection={unnoticed}
/>

<div class={["grid h-screen grid-rows-[1fr_auto]", isCollapsed ? "grid-cols-[0_1fr]" : "grid-cols-[16rem_1fr]"]}>
  <Sidebar
    {devices}
    {connections}
    {current}
    {connecting}
    collapsed={isCollapsed}
    {say}
    onselect={select}
    ondisconnect={disconnect}
    onforget={forget}
    onnew={() => showForm()}
  />

  <div class="grid min-w-0 overflow-hidden">
    <!-- We key by the connection itself, so one that takes the place of another (an open device connected to again)
         starts afresh. -->
    {#each Object.keys(connections) as host (connections[host])}
      <Device bind:connection={connections[host]} hidden={host !== current} {say} lost={(error) => lost(host, error)} />
    {/each}
    {#if current === null}
      {#key form.key}
        <Connect
          device={form.device}
          connecting={form.isConnecting}
          error={form.error}
          onsubmit={connect}
          ontrust={trust}
        />
      {/key}
    {/if}
  </div>

  <footer class="col-span-full flex h-8 items-center gap-2 border-t border-line bg-pane px-1.5">
    <button
      type="button"
      class="btn-icon size-7"
      title="Toggle the sidebar (⌘B)"
      onclick={() => (isCollapsed = !isCollapsed)}
    >
      <PanelLeft class="size-4" />
    </button>
    <!-- What is true now outlasts what just happened. -->
    {#if current !== null && connections[current].silentSince !== undefined}
      <output class="truncate text-muted">{current} isn't answering, trying again…</output>
    {:else}
      <!-- Selectable, and kept while the pointer is on it, to copy. -->
      <output
        class={["truncate select-text", message.isFailed ? "text-danger" : "text-muted"]}
        onmouseenter={() => clearTimeout(hideTimeout)}
        onmouseleave={linger}
      >
        {message.text}
      </output>
    {/if}
    <!-- Then what the device on screen says, on the right. -->
    {#if current !== null}{@render connections[current].footer?.()}{/if}
    <div class={["flex items-center gap-1", current === null && "ml-auto"]}>
      {#if update !== null}
        <button
          type="button"
          class="btn btn-secondary h-6 px-2"
          title="Installs Ooblin {update} and restarts, which closes every connection"
          onclick={installUpdate}
        >
          <Download class="size-3.5" /> Update to {update}
        </button>
      {/if}
      <button type="button" class="btn-icon size-7" title="Help" popovertarget="help">
        <CircleHelp class="size-4" />
      </button>
    </div>
    <div
      id="help"
      popover
      class="fixed inset-auto right-1.5 bottom-9 m-0 min-w-52 rounded-lg border border-line bg-canvas p-1 text-ink shadow-xl"
    >
      <button
        type="button"
        class="block h-8 w-full rounded-md px-3 text-left hover:bg-accent hover:text-white"
        popovertarget="help"
        popovertargetaction="hide"
        title="Looks for a newer release of Ooblin."
        onclick={() => checkUpdate(true)}
      >
        Check for updates
      </button>
      <button
        type="button"
        class="block h-8 w-full rounded-md px-3 text-left hover:bg-accent hover:text-white"
        popovertarget="help"
        popovertargetaction="hide"
        title="Saves what Ooblin did and what went wrong, with the devices' addresses and names, never a password."
        onclick={saveLog}
      >
        Save log…
      </button>
    </div>
  </footer>
</div>
