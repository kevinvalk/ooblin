<!-- A device we're connected to: its power, the disk image in its drive and a tab at a time. There's one per open device, shown or
     not, so all of it stays as it was while another one is: pages until refreshed, the screen and serial console open.
     Whether a device that doesn't answer is gone is App's to say. -->
<script module lang="ts">
  import type { Snippet } from "svelte";
  import type { CommandError, Drive, Mounted } from "../bindings/ooblin";
  import type { Info, Power, PowerAction, PowerState, RemoteAccess, Settings as SettingsData } from "../bindings/core";

  /** What App shows of a connection too: in the sidebar, the title and the footer. */
  export interface Connection {
    host: string;
    info: Info;
    power?: Power;
    /** Since when it hasn't answered, see Device's `readPower`. */
    silent?: number;
    /** Set from a power change until the state moves or a minute has passed, a reset never shows as another state. */
    changing?: { from: PowerState; until: number };
    /** What it says in the footer while it's on screen. */
    footer?: Snippet;
  }

  /** The stretches a disk image is shown in, as read, and how fast AMT reads one at most, bytes a second. */
  const CELLS = 128;
  const TOP = 1.1e6;

  /** Our commands fail with a `CommandError`, Tauri itself with a string. */
  export function reason(error: unknown) {
    return (error as CommandError).message ?? String(error);
  }
</script>

<script lang="ts">
  import { ChevronDown, Disc3, LoaderCircle, RefreshCw, X } from "@lucide/svelte";
  import { Channel, invoke } from "@tauri-apps/api/core";
  import { untrack } from "svelte";
  import { SvelteMap } from "svelte/reactivity";
  import AuditLog from "./tabs/AuditLog.svelte";
  import EventLog from "./tabs/EventLog.svelte";
  import Hardware from "./tabs/Hardware.svelte";
  import Kvm from "./tabs/Kvm.svelte";
  import Settings from "./tabs/Settings.svelte";
  import Sol from "./tabs/Sol.svelte";
  import Status from "./tabs/Status.svelte";

  let {
    connection = $bindable(),
    hidden,
    say,
    lost,
  }: {
    connection: Connection;
    hidden: boolean;
    say: (text: string, failed?: boolean) => void;
    lost: (error: CommandError) => void;
  } = $props();
  const id = $props.id();
  connection.footer = status;

  /** The commands are named after the tabs that read. All but the hardware change often enough to want a refresh. The
   * KVM tab is a screen of its own, for devices that have one. */
  const TABS = {
    status: { label: "Status", page: Status, live: true },
    hardware: { label: "Hardware", page: Hardware, live: false },
    event_log: { label: "Event log", page: EventLog, live: true },
    audit_log: { label: "Audit log", page: AuditLog, live: true },
    settings: { label: "Settings", page: Settings, live: true },
  };
  let tab = $state<keyof typeof TABS | "kvm" | "sol">("status");
  /** By tab: what it read, or why it couldn't, and when, as it came. */
  const pages = new SvelteMap<string, { data?: unknown; error?: string; read?: Date }>();
  let page = $derived(tab === "kvm" || tab === "sol" ? undefined : pages.get(tab));
  let live = $derived(tab !== "kvm" && tab !== "sol" && TABS[tab].live);
  /** Whether its screen and serial console were opened: they stay open across tabs. What they wait for while they're
   * not open, and the screen's size and updates a second while it's shown, for the footer. */
  let opened = $state({ kvm: false, sol: false });
  let waiting = $state({ kvm: "", sol: "" });
  let screen = $state<{ width: number; height: number; fps: number }>();

  /** Shows a tab: a screen or serial console opens, a page is read unless we have it. */
  function show(name: typeof tab) {
    tab = name;
    if (name === "kvm" || name === "sol") opened[name] = true;
    else if (!pages.has(name)) read(name);
  }
  read();

  function close(kind: "kvm" | "sol") {
    opened[kind] = false;
    if (tab === kind) show("status");
  }

  /** What the device lets us redirect, as the settings last said. */
  let access: RemoteAccess = $derived(
    (pages.get("settings")?.data as SettingsData | undefined)?.remote_access ?? connection.info.remote_access,
  );

  /** What Settings saved, which it then shows: the name, and what's turned off goes (Rust ejects the image itself). */
  function saved(data: SettingsData) {
    pages.set("settings", { data, read: new Date() });
    connection.info.name = data.general.name;
    opened.kvm &&= !!data.remote_access.kvm;
    opened.sol &&= data.remote_access.sol;
    if (!data.remote_access.ider) image = undefined;
  }

  /** Why the screen, serial console or a disk image can't open, if it can't: it's off until turned on in Settings. */
  function off(kind: "kvm" | "sol" | "ider") {
    if (kind === "kvm" && access.kvm === null) return "This device has no KVM.";
    if (access[kind]) return;
    const what = { kvm: "KVM is", sol: "The serial console is", ider: "Disk images are" }[kind];
    return `${what} turned off, in Settings under Remote access.`;
  }

  /** Reads the tab afresh. It starts blank, so a refresh shows it happened, and only the last read lands. */
  async function read(name = tab) {
    const blank = {};
    pages.set(name, blank);
    const page = await invoke(name, { host: connection.host }).then(
      (data) => ({ data, read: new Date() }),
      (error) => ({ error: reason(error) }),
    );
    if (pages.get(name) === blank) pages.set(name, page);
  }

  let dialog: HTMLDialogElement;
  let asking = $state({ question: "", answers: [] as string[], safe: false });

  /** Asks `question` in a dialog, with Cancel and `answers`, the last of which is the one to think twice about unless
   * `safe`. Says the answer, or nothing for Cancel. */
  function ask(question: string, answers: string[], safe = false) {
    asking = { question, answers, safe };
    dialog.returnValue = "";
    dialog.showModal();
    return new Promise<string>((resolve) =>
      dialog.addEventListener("close", () => resolve(dialog.returnValue), { once: true }),
    );
  }

  let consentDialog: HTMLDialogElement;
  /** What consent is asked for, and how sending the code goes. */
  let consenting = $state({ what: "", error: "", sending: false });
  /** Consent being asked for, which whoever needs it meanwhile waits for too, and how it's answered. */
  let consented: Promise<boolean> | undefined;
  let answer = (_given: boolean) => {};

  /** Asks someone at the device for consent: it shows a code on its screen, which they read out. */
  function consent(kind: "kvm" | "sol" | "ider") {
    const what = { kvm: "Seeing its screen", sol: "Its serial console", ider: "Putting a disk image in its drive" }[kind];
    consented ??= (async () => {
      try {
        await invoke("consent_start", { host: connection.host });
      } catch (error) {
        say(reason(error), true);
        return false;
      }
      consenting = { what, error: "", sending: false };
      consentDialog.querySelector("form")!.reset();
      consentDialog.showModal();
      return new Promise<boolean>((resolve) => (answer = resolve));
    })().finally(() => (consented = undefined));
    return consented;
  }

  async function sendCode(event: SubmitEvent) {
    event.preventDefault();
    // However it's typed: "123 456", say.
    const digits = String(new FormData(event.currentTarget as HTMLFormElement).get("code")).replace(/\D/g, "");
    if (digits.length !== 6) return (consenting.error = "The code has 6 digits.");
    consenting = { ...consenting, error: "", sending: true };
    try {
      await invoke("consent_send", { host: connection.host, code: Number(digits) });
      consentDialog.close();
      say("Consent given.");
      answer(true);
    } catch (error) {
      consenting = { ...consenting, error: reason(error), sending: false };
    }
  }

  /** The code goes off the device's screen too. */
  function decline() {
    invoke("consent_cancel", { host: connection.host }).catch(() => {});
    answer(false);
  }

  /** Its power, read every 15 seconds, shown or not, so changes from elsewhere (the OS shutting down, another console)
   * show up in the sidebar too. And at once when it's shown, and when the power menu opens, which offers what AMT can
   * do right now. One read at a time: a device that
   * doesn't answer takes a while to say so. It may not for a while (AMT can be busy while it resets), only after two
   * minutes is it `lost`, until then its power is unknown. */
  let reading = false;
  async function readPower() {
    if (reading) return;
    reading = true;
    try {
      connection.power = await invoke<Power>("power", { host: connection.host });
      connection.silent = undefined;
      const changing = connection.changing;
      if (changing && (connection.power.state !== changing.from || Date.now() > changing.until)) connection.changing = undefined;
    } catch (error) {
      if ((error as CommandError).kind !== "Unreachable") {
        if (!hidden) say(reason(error), true);
      } else if (Date.now() - (connection.silent ??= Date.now()) > 120_000) {
        lost(error as CommandError);
      } else {
        connection.power = undefined;
      }
    } finally {
      reading = false;
    }
  }
  $effect(() => {
    if (!hidden) untrack(readPower);
  });
  $effect(() => {
    const every = setInterval(readPower, 15_000);
    return () => clearInterval(every);
  });

  /** Powering on needs no second thought, everything else does. */
  async function choose(action: PowerAction, label: string) {
    if (!action.startsWith("On") && !(await ask(`${label} ${connection.info.name}?`, [label]))) return;
    try {
      await invoke("set_power", { host: connection.host, action });
      if (connection.power) connection.changing = { from: connection.power.state, until: Date.now() + 60_000 };
      say(`${label} sent.`);
      // The BIOS's screen goes there.
      if (action.endsWith("Serial")) show("sol");
    } catch (error) {
      say(reason(error), true);
    }
  }

  /** The disk image in its drive (see `Mounted`), how much of it the device read and how fast (bytes a second), and
   * when it last read each of its `CELLS` stretches (`Date.now()`, 0 for never). And whether one is on its way in. */
  let image: (Mounted & { read: number; rate: number; cells: number[] }) | undefined = $state();
  let mounting = $state(false);

  /** The image is read from here as the device asks for it, the file never goes through the webview. Rust tells us what
   * of it each read was, how fast it reads we work out every second while it does. */
  async function mount(path?: string | null) {
    path ??= await invoke<string | null>("pick", { kind: "Disk image", extensions: ["iso"] });
    if (!path) return;
    mounting = true;
    const mounted = $state({ id: 0, name: "", size: 0, read: 0, rate: 0, cells: Array<number>(CELLS).fill(0) });
    let [tick, last] = [0, 0];
    const drive = new Channel<Drive>((event) => {
      // Until it's in, and once it's out, it's not ours to show.
      if (image !== mounted) return;
      if (event.kind === "Ended") {
        image = undefined;
        if (event.error) return say(`${mounted.name} came out of the drive: ${reason(event.error)}`, true);
        return say(`${connection.info.name} closed the disk image (a reset, say), mount it again to use it.`);
      }
      mounted.read += event.end - event.start;
      const cell = (at: number) => (at * CELLS) / mounted.size;
      for (let i = Math.floor(cell(event.start)); i < cell(event.end); i++) mounted.cells[i] = Date.now();
      tick ||= setInterval(() => {
        [mounted.rate, last] = [mounted.read - last, mounted.read];
        if (mounted.rate) return;
        clearInterval(tick);
        tick = 0;
      }, 1000);
    });
    try {
      Object.assign(mounted, await invoke<Mounted>("ider_mount", { host: connection.host, path, drive }));
      image = mounted;
      say(`${mounted.name} is in the drive, the device sees it from its next boot on.`);
    } catch (error) {
      if ((error as CommandError).kind === "Consent" && (await consent("ider"))) return await mount(path);
      say(reason(error), true);
    } finally {
      mounting = false;
    }
  }

  function eject() {
    invoke("ider_eject", { host: connection.host, id: image!.id });
    image = undefined;
  }

  /** Moves between the tabs as tabs do: the arrow keys to the one before or after, Home and End to the first or last. */
  function arrows(event: KeyboardEvent & { currentTarget: HTMLElement }) {
    const tabs = [...event.currentTarget.querySelectorAll<HTMLElement>("[role=tab]")];
    const at = tabs.indexOf(document.activeElement as HTMLElement);
    const to = { ArrowLeft: at - 1, ArrowRight: at + 1, Home: 0, End: -1 }[event.key];
    if (at < 0 || to === undefined) return;
    event.preventDefault();
    const next = tabs.at(to % tabs.length)!;
    next.focus();
    next.click();
  }

  // ⌘R would reload the whole app, we read the tab on screen again instead.
  function keydown(event: KeyboardEvent) {
    if (!hidden && (event.metaKey || event.ctrlKey) && event.key === "r") {
      event.preventDefault();
      if (live) read();
    }
  }
</script>

<svelte:window onkeydown={keydown} />

<!-- A tab that's `off` looks it, and says why when clicked. Only the one shown is in the tab order, the arrow keys move
     between them (see `arrows`). -->
{#snippet tabButton(name: typeof tab, label: string, off?: string)}
  <button
    type="button"
    role="tab"
    id="{id}-{name}"
    class="-mb-px h-10 border-b-2 border-transparent text-muted hover:text-ink aria-selected:border-accent aria-selected:text-ink"
    aria-selected={name === tab}
    aria-controls="{id}-{name === 'kvm' || name === 'sol' ? name : 'page'}-panel"
    aria-disabled={!!off}
    tabindex={name === tab ? 0 : -1}
    title={off}
    onclick={() => (off ? say(off) : show(name))}
  >
    {label}
  </button>
{/snippet}

<!-- An open screen or serial console stays open across tabs, until closed here. -->
{#snippet closeButton(kind: "kvm" | "sol", help: string)}
  {#if opened[kind]}
    <button
      type="button"
      class="btn-icon -ml-5 size-5"
      title={help}
      onclick={() => close(kind)}
    >
      <X class="size-3.5" />
    </button>
  {/if}
{/snippet}

{#snippet action(name: PowerAction, label: string, help: string)}
  <button
    type="button"
    class="block h-8 w-full rounded-md px-3 text-left hover:bg-accent hover:text-white disabled:text-muted"
    popovertarget="{id}-power"
    popovertargetaction="hide"
    disabled={!connection.power?.actions.includes(name) ||
      (name.endsWith("Image") && !image) ||
      (name.endsWith("Serial") && !!off("sol"))}
    title={help}
    onclick={() => choose(name, label)}
  >
    {label}
  </button>
{/snippet}

<!-- What the footer says of it while it's on screen: what its screen and serial console wait for, how large its screen
     is, and its disk image as the device reads it: how fast, from red to green at the top speed, and like a
     defragmenter's map which stretches, fading for as long as reading all of it at the top speed would take, so a whole
     read leaves a trail across them. -->
{#snippet status()}
  {@const opening = waiting.kvm || waiting.sol}
  {#if opening}
    <output class="flex min-w-0 items-center gap-1.5 text-muted">
      <LoaderCircle class="size-3.5 flex-none animate-spin" /><span class="truncate">{opening}</span>
    </output>
  {/if}
  <div class="ml-auto flex items-center gap-6 pr-1.5 whitespace-nowrap text-muted tabular-nums">
    {#if screen}
      <span title="The device's screen, and the updates a second it sends">
        KVM {screen.width} × {screen.height}, {screen.fps} fps
      </span>
    {/if}
    {#if image}
      {@const speed = Math.min(image.rate / TOP, 1)}
      <div
        class="flex gap-0.75"
        title="Disk read {(image.rate / 1e6).toFixed(2)} MB/s which top out at about {TOP /
          1e6} MB/s as each read goes through a small management engine."
      >
        <div class="flex w-1 items-end outline outline-line">
          <div
            class="w-full transition-[height] duration-1000 ease-linear"
            style:height="{speed * 100}%"
            style:background-color="hsl({speed * 120} 80% 45%)"
          ></div>
        </div>
        <div
          class="grid grid-flow-col grid-rows-4 gap-px"
          style:--decay="{image.size / TOP}s"
        >
          {#each image.cells as read}
            <!-- Each read restarts its animation, which shown again goes on where it is by now. -->
            {#key read}<div
                class={["size-1 bg-line", read && "animate-[read_var(--decay)_linear]"]}
                style:animation-delay="{read - Date.now()}ms"
              ></div>{/key}
          {/each}
        </div>
      </div>
    {/if}
  </div>
{/snippet}

<section class="grid min-h-0 grid-rows-[auto_auto_1fr]" {hidden}>
  <header class="flex items-center gap-3 px-8 pt-6 pb-4">
    <span
      class={["dot", connection.changing && "animate-pulse"]}
      data-power={connection.power?.state}
      title={connection.power && connection.power.state + (connection.changing ? ", changing…" : "")}
    ></span>
    <h1 class="truncate text-xl font-semibold">{connection.info.name}</h1>
    <!-- The disk image in the device's drive, for booting from (IDER). -->
    {#if image}
      <!-- Lit while the device reads, like a drive's light. -->
      <span class="ml-auto flex min-w-0 items-center gap-2 text-muted" title="In the device's drive">
        <Disc3 class={["size-4 flex-none", image.rate && "text-accent"]} />
        <span class="truncate">{image.name}</span>
        <button type="button" class="btn-icon size-7 flex-none" title="Eject" onclick={eject}><X class="size-4" /></button>
      </span>
    {:else}
      <button
        type="button"
        class="btn btn-secondary ml-auto"
        disabled={mounting}
        aria-disabled={!!off("ider")}
        title={off("ider")}
        onclick={() => (off("ider") ? say(off("ider")!) : mount())}
      >
        {#if mounting}<LoaderCircle class="size-4 animate-spin" />{:else}<Disc3 class="size-4" />{/if} Disk image…
      </button>
    {/if}
    <button type="button" class="btn btn-secondary" popovertarget="{id}-power">
      Power <ChevronDown class="size-4" />
    </button>
    <!-- Grouped: power on, asking the OS, acting at once, and booting elsewhere this once. -->
    <div
      id="{id}-power"
      ontoggle={(event) => event.newState === "open" && readPower()}
      popover
      class="fixed inset-auto top-15 right-8 m-0 min-w-52 rounded-lg border border-line bg-canvas p-1 text-ink shadow-xl [&>hr]:my-1 [&>hr]:border-line"
    >
      {@render action("On", "Power on", "Turns the device on.")}
      <hr />
      {@render action("SoftReset", "Restart", "Asks the OS to restart, while AMT can ask it.")}
      {@render action("SoftOff", "Shut down", "Asks the OS to shut down, while AMT can ask it.")}
      {@render action("Sleep", "Sleep", "Asks the OS to sleep, while AMT can ask it.")}
      {@render action("Hibernate", "Hibernate", "Asks the OS to hibernate, while AMT can ask it.")}
      <hr />
      {@render action("Reset", "Reset", "Restarts at once, like the reset button. Unsaved work is lost.")}
      {@render action("Cycle", "Power cycle", "Cuts the power, then turns it back on.")}
      {@render action("Off", "Power off", "Cuts the power at once, like holding the power button.")}
      {@render action("Nmi", "Send NMI", "Sends a non-maskable interrupt, on which most OSes write a crash dump.")}
      <hr />
      {@render action("OnToBios", "Power on to BIOS", "Powers on into the BIOS setup, this once.")}
      {@render action("ResetToBios", "Reset to BIOS", "Resets at once into the BIOS setup, this once.")}
      {@render action("OnToPxe", "Power on to PXE", "Powers on and boots from the network, this once.")}
      {@render action("ResetToPxe", "Reset to PXE", "Resets at once and boots from the network, this once.")}
      {@render action("OnToImage", "Power on to disk image", "Powers on and boots from the image, this once.")}
      {@render action("ResetToImage", "Reset to disk image", "Resets at once and boots from the image, this once.")}
      {@render action("OnToSerial", "Power on to serial console", "Powers on with the BIOS's screen on the serial console, this once.")}
      {@render action("ResetToSerial", "Reset to serial console", "Resets at once with the BIOS's screen on the serial console, this once.")}
    </div>
  </header>

  <div class="mx-8 flex items-center gap-6 border-b border-line" role="tablist" tabindex="-1" onkeydown={arrows}>
    {#each Object.entries(TABS) as [name, { label }] (name)}
      {#if name !== "settings"}{@render tabButton(name as keyof typeof TABS, label)}{/if}
    {/each}
    {@render tabButton("kvm", "KVM", off("kvm"))}
    {@render closeButton("kvm", "Close the screen")}
    {@render tabButton("sol", "Serial", off("sol"))}
    {@render closeButton("sol", "Close the serial console")}
    <!-- Last, as what's looked at least. -->
    {@render tabButton("settings", "Settings")}
    {#if live}
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

  {#if opened.kvm}
    <Kvm
      id="{id}-kvm-panel"
      host={connection.host}
      hidden={hidden || tab !== "kvm"}
      consent={() => consent("kvm")}
      status={(text) => (waiting.kvm = text)}
      {say}
      measured={(measured) => (screen = measured)}
    />
  {/if}
  {#if opened.sol}
    <Sol
      id="{id}-sol-panel"
      host={connection.host}
      hidden={hidden || tab !== "sol"}
      consent={() => consent("sol")}
      status={(text) => (waiting.sol = text)}
    />
  {/if}
  {#if tab !== "kvm" && tab !== "sol"}
    {@const Page = TABS[tab].page}
    <div
      id="{id}-page-panel"
      class="overflow-y-auto px-8 pb-8 @container scrollbar-gutter-stable"
      role="tabpanel"
      aria-labelledby="{id}-{tab}"
    >
      <div class="mx-auto grid max-w-5xl gap-12 pt-6" aria-busy={!page?.data && !page?.error}>
        {#if page?.error}
          <div class="grid justify-items-center gap-4 py-16 text-center">
            <p class="text-muted">{page.error}</p>
            <button type="button" class="btn btn-secondary" onclick={() => read()}>Try again</button>
          </div>
        {:else}
          <!-- Each page knows the shape of its own data, the ones that change something take the rest too. -->
          <Page data={page?.data as never} {connection} {say} {ask} {saved} />
        {/if}
      </div>
    </div>
  {/if}
</section>

<dialog bind:this={dialog}>
  <form method="dialog" class="grid gap-5">
    <p class="font-medium">{asking.question}</p>
    <div class="flex justify-end gap-2">
      <button value="" class="btn btn-secondary">Cancel</button>
      {#each asking.answers as answer, i (answer)}
        {@const last = i === asking.answers.length - 1}
        <button value={answer} class={["btn", !last ? "btn-secondary" : asking.safe ? "btn-primary" : "bg-danger text-white hover:bg-danger/90"]}>
          {answer}
        </button>
      {/each}
    </div>
  </form>
</dialog>

<!-- Escape is a no, like Cancel. -->
<dialog bind:this={consentDialog} oncancel={decline}>
  <form class="grid gap-4" onsubmit={sendCode}>
    <p class="font-medium">Ask someone at {connection.info.name}</p>
    <p>
      {consenting.what} needs the consent of someone at the device. It shows a 6-digit code on its screen now: have them
      read it out to you.
    </p>
    <input
      name="code"
      class="field w-full text-center text-lg tracking-[0.5em] tabular-nums"
      inputmode="numeric"
      autocomplete="off"
      placeholder="000000"
      required
    />
    {#if consenting.error}<p class="text-danger">{consenting.error}</p>{/if}
    <div class="flex justify-end gap-2">
      <button type="button" class="btn btn-secondary" onclick={() => (consentDialog.close(), decline())}>Cancel</button>
      <button class="btn btn-primary" disabled={consenting.sending}>
        {#if consenting.sending}<LoaderCircle class="size-4 animate-spin" />{/if} Send
      </button>
    </div>
  </form>
</dialog>
