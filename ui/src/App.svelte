<!-- The window: devices on the left, the connect form or a session on the right, a status line at the bottom. -->
<script lang="ts">
  import { LoaderCircle, PanelLeft, Trash2, X } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { CommandError, Device } from "./bindings/console";
  import type { Info, Power } from "./bindings/core";
  import Connect from "./Connect.svelte";
  import Session, { type Session as Open } from "./Session.svelte";

  /** Every device we logged in to, the open ones by host, and the one on screen (none: the connect form). */
  let devices: Device[] = $state([]);
  let sessions: Record<string, Open> = $state({});
  let current: string | null = $state(null);
  /** The connect form, for `device` if given: a new `key` is a fresh one. `error` is why the last attempt failed. */
  let form = $state({ key: 0, device: undefined as Device | undefined, error: undefined as CommandError | undefined });
  /** The device we're connecting to: it spins, and a second click on it does nothing. */
  let connecting: string | null = $state(null);
  let collapsed = $state(false);
  /** The disk image in the drive of the device on screen, see Session. */
  let image = $derived(current ? sessions[current]?.image : undefined);
  let message = $state("");
  let hide = 0;

  /** Says `text` in the footer for as long as it takes to read: five seconds, more for more words, fifteen at most. */
  function say(text: string) {
    message = text;
    clearTimeout(hide);
    hide = setTimeout(() => (message = ""), Math.min(5000 + 50 * text.length, 15_000));
  }

  async function listDevices() {
    devices = await invoke<Device[]>("devices").catch((error) => (say(String(error)), []));
  }

  /** Open sessions show, remembered devices log in, the rest ask for their password. */
  function select(device: Device) {
    if (sessions[device.host]) return show(device.host);
    if (connecting === device.host) return;
    showForm(device);
    if (device.remembered) connect({ host: device.host, user: device.user, password: null, remember: false });
  }

  function showForm(device?: Device, error?: CommandError) {
    form = { key: form.key + 1, device, error };
    current = null;
    say("");
    listDevices();
  }

  /** Without a `password` with the one we have, see the `connect` command. */
  async function connect(login: { host: string; user: string; password: string | null; remember: boolean }) {
    const host = login.host;
    connecting = host;
    form.error = undefined;
    try {
      sessions[host] = { host, info: await invoke<Info>("connect", login), pages: {} };
      show(host);
    } catch (error) {
      form.error = explain(error, `Couldn't reach ${host}`);
    } finally {
      connecting = null;
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
    refreshPower(host);
    listDevices();
  }

  async function disconnect(host: string) {
    await invoke("disconnect", { host });
    delete sessions[host];
    if (host === current) showForm();
  }

  async function forget(host: string) {
    await invoke("forget", { host });
    listDevices();
  }

  /** The device stopped answering for good: we let go of it and say why, Connect tries again. */
  function lost(host: string, error: CommandError) {
    invoke("disconnect", { host });
    delete sessions[host];
    const failure = explain(error, `Lost the connection to ${host}`);
    if (host === current) showForm(devices.find((device) => device.host === host), failure);
    else say(failure.message);
  }

  /** The hosts whose power we're asking for, one ask at a time: one that doesn't answer takes a while to say so. */
  const asking = new Set<string>();

  /** A device may not answer for a while (AMT can be busy while it resets), only after two minutes is it gone. Until
   * then its power is unknown. */
  async function refreshPower(host: string) {
    const session = sessions[host];
    if (asking.has(host)) return;
    asking.add(host);
    try {
      session.power = await invoke<Power>("power", { host });
      session.silent = undefined;
      const changing = session.changing;
      if (changing && (session.power.state !== changing.from || Date.now() > changing.until)) {
        session.changing = undefined;
      }
    } catch (error) {
      if ((error as CommandError).kind !== "Unreachable") {
        if (host === current) say(explain(error, host).message);
      } else if (Date.now() - (session.silent ??= Date.now()) > 120_000) {
        lost(host, error as CommandError);
      } else {
        session.power = undefined;
      }
    } finally {
      asking.delete(host);
    }
  }

  // The window is titled after the session on screen.
  $effect(() => {
    invoke("title", { title: current ? `${sessions[current].info.name} – AMT Console` : "AMT Console" });
  });

  // Power changes from elsewhere (the OS shutting down, another console) show up within a few seconds.
  setInterval(() => Object.keys(sessions).forEach(refreshPower), 15_000);

  function keydown(event: KeyboardEvent) {
    if ((event.metaKey || event.ctrlKey) && event.key === "b") {
      event.preventDefault();
      collapsed = !collapsed;
    }
  }

  // Started with a host on the command line? Open it as if it was picked from the sidebar.
  invoke<Device | null>("launch").then((device) => (device ? select(device) : showForm()));
</script>

<svelte:window onkeydown={keydown} />

<div class={["grid h-screen grid-rows-[1fr_auto]", collapsed ? "grid-cols-[0_1fr]" : "grid-cols-[16rem_1fr]"]}>
  <nav class="flex min-w-0 flex-col gap-3 overflow-hidden border-r border-line bg-pane p-3" class:invisible={collapsed}>
    <p class="px-2.5 pt-1 font-semibold text-muted">Devices</p>
    {#if devices.length}
      <ul class="grid flex-1 content-start gap-1 overflow-y-auto">
        {#each devices as device (device.host)}
          {@const open = sessions[device.host]}
          <!-- One action per device, on hover: disconnect an open one, forget a closed one. -->
          <li
            class="group flex items-center rounded-md pr-0.5 text-muted hover:bg-hover data-open:text-ink aria-current:bg-accent aria-current:text-white"
            data-open={open || connecting === device.host ? "" : undefined}
            aria-current={device.host === current}
          >
            <button type="button" class="flex h-8 min-w-0 flex-1 items-center gap-2.5 px-2.5" onclick={() => select(device)}>
              {#if connecting === device.host}
                <LoaderCircle class="-mx-0.5 size-3.5 flex-none animate-spin" />
              {:else}
                <span
                  class="dot"
                  class:invisible={!open}
                  class:animate-pulse={open?.changing}
                  data-power={open?.power?.state}
                  title={open?.power?.state}
                ></span>
              {/if}
              <span class="truncate">{device.host}</span>
            </button>
            <button
              type="button"
              class="btn-icon hidden size-7 group-hover:grid group-aria-current:text-white/80 group-aria-current:hover:bg-white/20"
              title={open ? "Disconnect" : "Forget"}
              onclick={() => (open ? disconnect(device.host) : forget(device.host))}
            >
              {#if open}<X class="size-4" />{:else}<Trash2 class="size-4" />{/if}
            </button>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="grid flex-1 place-items-center text-muted">No devices yet</p>
    {/if}
    <button type="button" class="btn btn-secondary" onclick={() => showForm()}>New connection</button>
  </nav>

  <div class="grid min-w-0 overflow-hidden">
    {#if current}
      <Session session={sessions[current]} {say} />
    {:else}
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
    <button type="button" class="btn-icon size-7" title="Toggle the sidebar (⌘B)" onclick={() => (collapsed = !collapsed)}>
      <PanelLeft class="size-4" />
    </button>
    <!-- What's true now outlasts what just happened. -->
    <output class="truncate text-muted">
      {current && sessions[current].silent ? `${current} isn't answering, trying again…` : message}
    </output>
    <!-- Only while the device reads, and a minute after. -->
    {#if image?.recent}
      <span
        class="ml-auto pr-1.5 whitespace-nowrap text-muted tabular-nums"
        title="How fast the device reads the disk image. AMT tops out at about 1 MB/s: each read goes through its small
management engine, and the device waits on every one."
      >
        Disk image {(image.rate / 1e6).toFixed(2)} MB/s
      </span>
    {/if}
  </footer>
</div>
