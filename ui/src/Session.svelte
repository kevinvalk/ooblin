<!-- An open session: its power and a tab at a time. Pages stay until refreshed, so going back to a tab shows it at
     once. A device that stops answering is `onlost`. -->
<script module lang="ts">
  import type { Info, Power, PowerAction, PowerState } from "./bindings/core";

  export interface Session {
    host: string;
    info: Info;
    power?: Power;
    /** Set from a power change until the state moves or a minute has passed, a reset never shows as another state. */
    changing?: { from: PowerState; until: number };
    /** By tab: what it read, or why it couldn't, and when. */
    pages: Record<string, { data?: unknown; error?: string; read?: Date }>;
  }
</script>

<script lang="ts">
  import { ChevronDown, RefreshCw } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { CommandError } from "./bindings/console";
  import Audit from "./pages/Audit.svelte";
  import Events from "./pages/Events.svelte";
  import Hardware from "./pages/Hardware.svelte";
  import Settings from "./pages/Settings.svelte";
  import Status from "./pages/Status.svelte";

  let {
    session,
    say,
    onlost,
  }: { session: Session; say: (text: string) => void; onlost: (error: CommandError) => void } = $props();

  /** The commands are named after the tabs. Status and the logs change often enough to want a refresh. */
  const TABS = {
    status: { label: "Status", page: Status, live: true },
    hardware: { label: "Hardware", page: Hardware, live: false },
    events: { label: "Event log", page: Events, live: true },
    audit: { label: "Audit log", page: Audit, live: true },
    settings: { label: "Settings", page: Settings, live: false },
  };
  let tab: keyof typeof TABS = $state("status");
  let page = $derived(session.pages[tab]);
  let Page = $derived(TABS[tab].page);

  // Whatever is on screen gets read, unless we have it.
  $effect(() => {
    if (!session.pages[tab]) read();
  });

  /** Reads the tab afresh. It starts blank, so a refresh shows it happened, and replaces the page, so whatever the one
   * before it still reads lands nowhere. */
  async function read(name = tab) {
    session.pages[name] = {};
    const page = session.pages[name];
    try {
      page.data = await invoke(name, { host: session.host });
      page.read = new Date();
    } catch (error) {
      failed(error, (message) => (page.error = message));
    }
  }

  let dialog: HTMLDialogElement;
  let confirming = $state({ action: "on" as PowerAction, label: "" });

  /** Powering on needs no second thought, everything else does. */
  function choose(action: PowerAction, label: string) {
    if (action.startsWith("on")) return change(action, label);
    confirming = { action, label };
    dialog.returnValue = "";
    dialog.showModal();
  }

  async function change(action: PowerAction, label: string) {
    try {
      await invoke("set_power", { host: session.host, action });
      if (session.power) session.changing = { from: session.power.state, until: Date.now() + 60_000 };
      say(`${label} sent.`);
    } catch (error) {
      failed(error, say);
    }
  }

  /** A device that stopped answering is lost, anything else is said where it happened. */
  function failed(error: unknown, show: (message: string) => void) {
    const failure = error as CommandError;
    if (failure.kind === "unreachable") onlost(failure);
    else show(failure.message ?? String(error));
  }

  // ⌘R would reload the whole app, we read the tab again instead.
  function keydown(event: KeyboardEvent) {
    if ((event.metaKey || event.ctrlKey) && event.key === "r") {
      event.preventDefault();
      if (TABS[tab].live) read();
    }
  }
</script>

<svelte:window onkeydown={keydown} />

{#snippet action(name: PowerAction, label: string, help: string)}
  <button
    type="button"
    class="block h-8 w-full rounded-md px-3 text-left enabled:hover:bg-accent enabled:hover:text-white disabled:text-muted"
    popovertarget="power-menu"
    popovertargetaction="hide"
    disabled={!session.power?.actions.includes(name)}
    title={help}
    onclick={() => choose(name, label)}
  >
    {label}
  </button>
{/snippet}

<section class="grid min-h-0 grid-rows-[auto_auto_1fr]">
  <header class="flex items-center gap-3 px-8 pt-6 pb-4">
    <span
      class="dot"
      class:animate-pulse={session.changing}
      data-power={session.power?.state}
      title={session.power && session.power.state + (session.changing ? ", changing…" : "")}
    ></span>
    <h1 class="truncate text-xl font-semibold">{session.info.name}</h1>
    <button type="button" class="btn btn-secondary ml-auto" popovertarget="power-menu">
      Power <ChevronDown class="size-4" />
    </button>
    <!-- Grouped: power on, asking the OS, acting at once, and booting elsewhere this once. -->
    <div
      id="power-menu"
      popover
      class="fixed inset-auto top-15 right-8 m-0 min-w-52 rounded-lg border border-line bg-canvas p-1 text-ink shadow-xl [&>hr]:my-1 [&>hr]:border-line"
    >
      {@render action("on", "Power on", "Turns the device on.")}
      <hr />
      {@render action("softReset", "Restart", "Asks the OS to restart, while AMT can ask it.")}
      {@render action("softOff", "Shut down", "Asks the OS to shut down, while AMT can ask it.")}
      {@render action("sleep", "Sleep", "Asks the OS to sleep, while AMT can ask it.")}
      {@render action("hibernate", "Hibernate", "Asks the OS to hibernate, while AMT can ask it.")}
      <hr />
      {@render action("reset", "Reset", "Restarts at once, like the reset button. Unsaved work is lost.")}
      {@render action("cycle", "Power cycle", "Cuts the power, then turns it back on.")}
      {@render action("off", "Power off", "Cuts the power at once, like holding the power button.")}
      {@render action("nmi", "Send NMI", "Sends a non-maskable interrupt, on which most OSes write a crash dump.")}
      <hr />
      {@render action("onToBios", "Power on to BIOS", "Powers on into the BIOS setup, this once.")}
      {@render action("resetToBios", "Reset to BIOS", "Resets at once into the BIOS setup, this once.")}
      {@render action("onToPxe", "Power on to PXE", "Powers on and boots from the network, this once.")}
      {@render action("resetToPxe", "Reset to PXE", "Resets at once and boots from the network, this once.")}
    </div>
  </header>

  <div class="mx-8 flex items-center gap-6 border-b border-line" role="tablist">
    {#each Object.entries(TABS) as [name, { label }]}
      <button
        type="button"
        role="tab"
        class="-mb-px h-10 border-b-2 border-transparent text-muted hover:text-ink aria-selected:border-accent aria-selected:text-ink"
        aria-selected={name === tab}
        onclick={() => (tab = name as keyof typeof TABS)}
      >
        {label}
      </button>
    {/each}
    {#if TABS[tab].live}
      <span class="ml-auto text-muted">{page?.read ? `Read at ${page.read.toLocaleTimeString()}` : ""}</span>
      <button
        type="button"
        class="btn btn-secondary h-7 px-2.5"
        disabled={page && !page.data && !page.error}
        title="Read again (⌘R)"
        onclick={() => read()}
      >
        <RefreshCw class="size-4" /> Refresh
      </button>
    {/if}
  </div>

  <main class="overflow-y-auto px-8 pb-8 @container [scrollbar-gutter:stable]">
    <div class="mx-auto grid max-w-5xl gap-12 pt-6" aria-busy={!page?.data && !page?.error}>
      {#if page?.error}
        <div class="grid justify-items-center gap-4 py-16 text-center">
          <p class="text-muted">{page.error}</p>
          <button type="button" class="btn btn-secondary" onclick={() => read()}>Try again</button>
        </div>
      {:else}
        <!-- Each page knows the shape of its own data. -->
        <Page data={page?.data as never} />
      {/if}
    </div>
  </main>
</section>

<dialog
  bind:this={dialog}
  onclose={() => dialog.returnValue === "confirm" && change(confirming.action, confirming.label)}
  class="m-auto w-80 rounded-xl border border-line bg-canvas p-5 text-ink shadow-2xl backdrop:bg-black/30"
>
  <form method="dialog" class="grid gap-5">
    <p class="font-medium">{confirming.label} {session.info.name}?</p>
    <div class="flex justify-end gap-2">
      <button value="cancel" class="btn btn-secondary">Cancel</button>
      <button value="confirm" class="btn btn-danger">{confirming.label}</button>
    </div>
  </form>
</dialog>
