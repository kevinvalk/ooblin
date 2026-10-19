<!-- An open session: its power, the disk image in its drive and a tab at a time. Pages stay until refreshed, so going
     back to a tab shows it at once. Whether a device that doesn't answer is gone is App's to say. -->
<script module lang="ts">
  import type { CommandError } from "./bindings/console";
  import type { Info, Power, PowerAction, PowerState, RemoteAccess } from "./bindings/core";

  export interface Session {
    host: string;
    info: Info;
    power?: Power;
    /** Since when it hasn't answered, see App's `refreshPower`. */
    silent?: number;
    /** Set from a power change until the state moves or a minute has passed, a reset never shows as another state. */
    changing?: { from: PowerState; until: number };
    /** By tab: what it read, or why it couldn't, and when. */
    pages: Record<string, { data?: unknown; error?: string; read?: Date }>;
    /** The disk image in its drive: its name and size, how much of it the device read and how fast (bytes a second),
     * and when it last read each of its `CELLS` stretches (`Date.now()`, 0 for never). */
    image?: { name: string; size: number; read: number; rate: number; cells: number[] };
    /** Whether a disk image is on its way into its drive. */
    mounting?: boolean;
    /** Whether its screen and serial console were opened: they stay open across tabs. */
    kvm?: boolean;
    sol?: boolean;
    /** What they wait for while they're not open, for the footer. */
    status?: { kvm?: string; sol?: string };
    /** The screen's size and updates a second while it's shown, for the footer. */
    screen?: { width: number; height: number; fps: number };
  }

  /** The stretches a disk image is shown in, as read. */
  export const CELLS = 128;

  /** Our commands fail with a `CommandError`, Tauri itself with a string. */
  export function reason(error: unknown) {
    return (error as CommandError).message ?? String(error);
  }
</script>

<script lang="ts">
  import { ChevronDown, Disc3, LoaderCircle, RefreshCw, X } from "@lucide/svelte";
  import { Channel, invoke } from "@tauri-apps/api/core";
  import AuditLog from "./pages/AuditLog.svelte";
  import EventLog from "./pages/EventLog.svelte";
  import Hardware from "./pages/Hardware.svelte";
  import Kvm from "./pages/Kvm.svelte";
  import Settings from "./pages/Settings.svelte";
  import Sol from "./pages/Sol.svelte";
  import Status from "./pages/Status.svelte";

  let { session = $bindable(), say }: { session: Session; say: (text: string, failed?: boolean) => void } = $props();

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
  let page = $derived(tab === "kvm" || tab === "sol" ? undefined : session.pages[tab]);
  let live = $derived(tab !== "kvm" && tab !== "sol" && TABS[tab].live);

  // Whatever is on screen gets read, unless we have it. A screen or serial console, once opened, stays open.
  $effect(() => {
    if (tab === "kvm" || tab === "sol") session[tab] = true;
    else if (!session.pages[tab]) read();
  });

  function close(kind: "kvm" | "sol") {
    session[kind] = false;
    if (tab === kind) tab = "status";
  }

  /** What the device lets us redirect, as the settings last said. */
  let access: RemoteAccess = $derived(
    (session.pages.settings?.data as { remote_access?: RemoteAccess } | undefined)?.remote_access ??
      session.info.remote_access,
  );

  /** Why the screen, serial console or a disk image can't open, if it can't: it's off until turned on in Settings. */
  function off(kind: "kvm" | "sol" | "ider") {
    if (kind === "kvm" && access.kvm === null) return "This device has no KVM.";
    if (access[kind]) return;
    const what = { kvm: "KVM is", sol: "The serial console is", ider: "Disk images are" }[kind];
    return `${what} turned off, in Settings under Remote access.`;
  }

  /** Reads the tab afresh. It starts blank, so a refresh shows it happened, and replaces the page, so whatever the one
   * before it still reads lands nowhere. */
  async function read(name = tab) {
    session.pages[name] = {};
    const page = session.pages[name];
    try {
      page.data = await invoke(name, { host: session.host });
      page.read = new Date();
    } catch (error) {
      page.error = reason(error);
    }
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
        await invoke("consent_start", { host: session.host });
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
      await invoke("consent_send", { host: session.host, code: Number(digits) });
      consentDialog.close();
      say("Consent given.");
      answer(true);
    } catch (error) {
      consenting = { ...consenting, error: reason(error), sending: false };
    }
  }

  /** The code goes off the device's screen too. */
  function decline() {
    invoke("consent_cancel", { host: session.host }).catch(() => {});
    answer(false);
  }

  /** Powering on needs no second thought, everything else does. */
  async function choose(action: PowerAction, label: string) {
    if (action.startsWith("On") || (await ask(`${label} ${session.info.name}?`, [label]))) change(action, label);
  }

  async function change(action: PowerAction, label: string) {
    try {
      await invoke("set_power", { host: session.host, action });
      if (session.power) session.changing = { from: session.power.state, until: Date.now() + 60_000 };
      say(`${label} sent.`);
      // The BIOS's screen goes there.
      if (action.endsWith("Serial")) tab = "sol";
    } catch (error) {
      say(reason(error), true);
    }
  }

  /** The image is read from here as the device asks for it, the file never goes through the webview. Rust tells us what
   * of it each read was, how fast it reads we work out every second while it does. All of it for the device it went
   * into, whichever one is shown by then. */
  async function mount(path?: string | null) {
    path ??= await invoke<string | null>("pick", { kind: "Disk image", extensions: ["iso"] });
    if (!path) return;
    const device = session;
    device.mounting = true;
    const image = $state({ name: "", size: 0, read: 0, rate: 0, cells: Array<number>(CELLS).fill(0) });
    let [tick, last] = [0, 0];
    const reads = new Channel<{ start: number; end: number } | null>();
    reads.onmessage = (read) => {
      // Until it's in, and once it's out, it's not ours to show.
      if (device.image !== image) return;
      if (!read) {
        device.image = undefined;
        return say(`${device.info.name} closed the disk image (a reset, say), mount it again to use it.`);
      }
      image.read += read.end - read.start;
      const cell = (at: number) => (at * CELLS) / image.size;
      for (let i = Math.floor(cell(read.start)); i < cell(read.end); i++) image.cells[i] = Date.now();
      tick ||= setInterval(() => {
        [image.rate, last] = [image.read - last, image.read];
        if (image.rate) return;
        clearInterval(tick);
        tick = 0;
      }, 1000);
    };
    try {
      [image.name, image.size] = await invoke<[string, number]>("ider_mount", { host: device.host, path, reads });
      device.image = image;
      say(`${image.name} is in the drive, the device sees it from its next boot on.`);
    } catch (error) {
      if ((error as CommandError).kind === "Consent" && (await consent("ider"))) return await mount(path);
      say(reason(error), true);
    } finally {
      device.mounting = false;
    }
  }

  function eject() {
    invoke("ider_eject", { host: session.host });
    session.image = undefined;
  }

  // ⌘R would reload the whole app, we read the tab again instead.
  function keydown(event: KeyboardEvent) {
    if ((event.metaKey || event.ctrlKey) && event.key === "r") {
      event.preventDefault();
      if (live) read();
    }
  }
</script>

<svelte:window onkeydown={keydown} />

<!-- A tab that's `off` looks it, and says why when clicked. -->
{#snippet tabButton(name: typeof tab, label: string, off?: string)}
  <button
    type="button"
    role="tab"
    class="-mb-px h-10 border-b-2 border-transparent text-muted hover:text-ink aria-selected:border-accent aria-selected:text-ink"
    aria-selected={name === tab}
    aria-disabled={!!off}
    title={off}
    onclick={() => (off ? say(off) : (tab = name))}
  >
    {label}
  </button>
{/snippet}

<!-- An open screen or serial console stays open across tabs, until closed here. -->
{#snippet closeButton(kind: "kvm" | "sol", help: string)}
  {#if session[kind]}
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
    popovertarget="power-menu"
    popovertargetaction="hide"
    disabled={!session.power?.actions.includes(name) ||
      (name.endsWith("Image") && !session.image) ||
      (name.endsWith("Serial") && !!off("sol"))}
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
    <!-- The disk image in the device's drive, for booting from (IDER). -->
    {#if session.image}
      <!-- Lit while the device reads, like a drive's light. -->
      <span class="ml-auto flex min-w-0 items-center gap-2 text-muted" title="In the device's drive">
        <Disc3 class={["size-4 flex-none", session.image.rate && "text-accent"]} />
        <span class="truncate">{session.image.name}</span>
        <button type="button" class="btn-icon size-7 flex-none" title="Eject" onclick={eject}><X class="size-4" /></button>
      </span>
    {:else}
      <button
        type="button"
        class="btn btn-secondary ml-auto"
        disabled={session.mounting}
        aria-disabled={!!off("ider")}
        title={off("ider")}
        onclick={() => (off("ider") ? say(off("ider")!) : mount())}
      >
        {#if session.mounting}<LoaderCircle class="size-4 animate-spin" />{:else}<Disc3 class="size-4" />{/if} Disk image…
      </button>
    {/if}
    <button type="button" class="btn btn-secondary" popovertarget="power-menu">
      Power <ChevronDown class="size-4" />
    </button>
    <!-- Grouped: power on, asking the OS, acting at once, and booting elsewhere this once. -->
    <div
      id="power-menu"
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

  <div class="mx-8 flex items-center gap-6 border-b border-line" role="tablist">
    {#each Object.entries(TABS) as [name, { label }]}
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

  {#if session.kvm}
    <Kvm
      host={session.host}
      hidden={tab !== "kvm"}
      consent={() => consent("kvm")}
      status={(text) => ((session.status ??= {}).kvm = text)}
      measured={(screen) => (session.screen = screen)}
    />
  {/if}
  {#if session.sol}
    <Sol
      host={session.host}
      hidden={tab !== "sol"}
      consent={() => consent("sol")}
      status={(text) => ((session.status ??= {}).sol = text)}
    />
  {/if}
  {#if tab !== "kvm" && tab !== "sol"}
    {@const Page = TABS[tab].page}
    <main class="overflow-y-auto px-8 pb-8 @container [scrollbar-gutter:stable]">
      <div class="mx-auto grid max-w-5xl gap-12 pt-6" aria-busy={!page?.data && !page?.error}>
        {#if page?.error}
          <div class="grid justify-items-center gap-4 py-16 text-center">
            <p class="text-muted">{page.error}</p>
            <button type="button" class="btn btn-secondary" onclick={() => read()}>Try again</button>
          </div>
        {:else}
          <!-- Each page knows the shape of its own data, the ones that change something take the rest too. -->
          <Page data={page?.data as never} {session} {say} {ask} />
        {/if}
      </div>
    </main>
  {/if}
</section>

<dialog bind:this={dialog} class="dialog w-96">
  <form method="dialog" class="grid gap-5">
    <p class="font-medium">{asking.question}</p>
    <div class="flex justify-end gap-2">
      <button value="" class="btn btn-secondary">Cancel</button>
      {#each asking.answers as answer, i}
        {@const last = i === asking.answers.length - 1}
        <button value={answer} class={["btn", !last ? "btn-secondary" : asking.safe ? "btn-primary" : "btn-danger"]}>
          {answer}
        </button>
      {/each}
    </div>
  </form>
</dialog>

<!-- Escape is a no, like Cancel. -->
<dialog bind:this={consentDialog} class="dialog w-96" oncancel={decline}>
  <form class="grid gap-4" onsubmit={sendCode}>
    <p class="font-medium">Ask someone at {session.info.name}</p>
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
