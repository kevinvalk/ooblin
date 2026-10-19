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
  let message = $state("");

  async function listDevices() {
    devices = await invoke<Device[]>("devices").catch((error) => ((message = String(error)), []));
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
    message = "";
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
    message = "";
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

  /** The device stopped answering, even to a second try: we let go of it and say why, Connect tries again. */
  function lost(host: string, error: CommandError) {
    invoke("disconnect", { host });
    delete sessions[host];
    const failure = explain(error, `Lost the connection to ${host}`);
    if (host === current) showForm(devices.find((device) => device.host === host), failure);
    else message = failure.message;
  }

  async function refreshPower(host: string) {
    const session = sessions[host];
    try {
      session.power = await invoke<Power>("power", { host });
      const changing = session.changing;
      if (changing && (session.power.state !== changing.from || Date.now() > changing.until)) {
        session.changing = undefined;
      }
    } catch (error) {
      if ((error as CommandError).kind === "Unreachable") lost(host, error as CommandError);
      else if (host === current) message = explain(error, host).message;
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
      <Session
        session={sessions[current]}
        say={(text) => (message = text)}
        onlost={(error) => lost(current!, error)}
      />
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
    <output class="truncate text-muted">{message}</output>
  </footer>
</div>
