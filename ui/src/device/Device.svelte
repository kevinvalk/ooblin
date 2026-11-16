<script module lang="ts">
  import type { Component } from "svelte";
  import type { Changed, CommandError, Drive, Mounted } from "../bindings/ooblin";
  import type { BmcChange, Change, Flavor, IpmiChange, Power, PowerAction, Settings as SettingsData } from "../bindings/core";

  const CELLS = 128; // The stretches we show a disk image in, as the device reads it

  // The top of the speed meter is how fast AMT reads an image at most, as we do not know how fast any BMC does.
  const TOP_BYTES_PER_SECOND = 1.1e6;
</script>

<script lang="ts">
  import { ChevronDown, Disc3, LoaderCircle, RefreshCw, X } from "@lucide/svelte";
  import { Channel, invoke } from "@tauri-apps/api/core";
  import { untrack } from "svelte";
  import { SvelteMap } from "svelte/reactivity";
  import { reason, type Connection } from "../connection";
  import AuditLog from "./amt/AuditLog.svelte";
  import AmtHardware from "./amt/Hardware.svelte";
  import AmtSettings from "./amt/Settings.svelte";
  import AmtStatus from "./amt/Status.svelte";
  import EventLog from "./EventLog.svelte";
  import IpmiHardware from "./ipmi/Hardware.svelte";
  import IpmiSettings from "./ipmi/Settings.svelte";
  import IpmiStatus from "./ipmi/Status.svelte";
  import Kvm from "./Kvm.svelte";
  import Panel from "./ipmi/Panel.svelte";
  import Sensors from "./ipmi/Sensors.svelte";
  import Sol from "./Sol.svelte";
  import RedfishHardware from "./redfish/Hardware.svelte";
  import RedfishStatus from "./redfish/Status.svelte";
  import BmcSettings from "./supermicro/Settings.svelte";

  // NOTE: There is one of us per open device, shown or not, so that all of it stays as it was while another one is on
  // screen: pages until refreshed, the screen and serial console open. Whether a device that does not answer is gone
  // is for App to say.
  let {
    connection = $bindable(),
    hidden,
    say: tell,
    lost,
  }: {
    connection: Connection;
    hidden: boolean;
    say: (text: string, isFailed?: boolean) => void;
    lost: (error: CommandError) => void;
  } = $props();
  const id = $props.id();
  connection.footer = status;

  let isAmt = $derived(connection.info.flavor === "Amt");

  /** Its name, or the address we reach it at for a device without one. */
  let title = $derived(connection.info.name || connection.host);

  /** What we say while another device is on screen names us. */
  const say = (text: string, isFailed?: boolean) => tell(hidden ? `${title}: ${text}` : text, isFailed);

  /** The tabs of a flavor, with its pages in its own folder or that of the standard it speaks. A tab is named after the
   * command that reads it, and the settings come last where a flavor has them, which is how we tell them. All but the
   * hardware (`isFixed`) change often enough to want a refresh, sensors so often that we read them every `everyMs`
   * while they are shown. The KVM and serial console are screens of their own. */
  const TABS: Record<
    Flavor,
    Record<string, { label: string; page: Component<any>; isFixed?: true; everyMs?: number }>
  > = {
    Amt: {
      status: { label: "Status", page: AmtStatus },
      hardware: { label: "Hardware", page: AmtHardware, isFixed: true },
      event_log: { label: "Event log", page: EventLog },
      audit_log: { label: "Audit log", page: AuditLog },
      settings: { label: "Settings", page: AmtSettings },
    },
    Supermicro: {
      bmc_status: { label: "Status", page: RedfishStatus },
      hardware: { label: "Hardware", page: RedfishHardware, isFixed: true },
      event_log: { label: "Event log", page: EventLog },
      sensors: { label: "Sensors", page: Sensors, everyMs: 1000 },
      bmc_settings: { label: "Settings", page: BmcSettings },
    },
    Redfish: {
      bmc_status: { label: "Status", page: RedfishStatus },
      hardware: { label: "Hardware", page: RedfishHardware, isFixed: true },
      event_log: { label: "Event log", page: EventLog },
      // Redfish has a resource per sensor, so a read takes a BMC a while.
      sensors: { label: "Sensors", page: Sensors, everyMs: 5000 },
    },
    Ipmi: {
      ipmi_status: { label: "Status", page: IpmiStatus },
      fru: { label: "Hardware", page: IpmiHardware, isFixed: true },
      event_log: { label: "Event log", page: EventLog },
      sensors: { label: "Sensors", page: Sensors, everyMs: 1000 },
      ipmi_settings: { label: "Settings", page: IpmiSettings },
    },
  };
  let tabs = $derived(TABS[connection.info.flavor]);
  let names = $derived(Object.keys(tabs));
  let tab = $state(untrack(() => names[0]));

  /** By tab: what it read or why it could not, and when. */
  const pages = new SvelteMap<string, { data?: unknown; error?: string; read?: Date }>();
  let page = $derived(pages.get(tab));
  let isLive = $derived(tabs[tab] && !tabs[tab].isFixed);

  /** Whether its screen and serial console were opened, as they stay open across tabs. Then, for the footer: what they
   * wait for while they are not open yet, and the size of the screen and its updates a second while it is shown. */
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
    if (tab === kind) show(names[0]);
  }

  let isSaving = $state(false);

  /** Makes a settings `change` and shows the settings after, which we read again however it went, as a change that
   * failed may have made some of it. The footer says how it went (`done` if it did) and we return whether it went
   * through. */
  async function save(done: string, change: Change | BmcChange | IpmiChange) {
    isSaving = true;
    try {
      const commands: Partial<Record<Flavor, string>> = { Amt: "change", Supermicro: "bmc_change", Ipmi: "ipmi_change" };
      const { error, settings } = await invoke<Changed<unknown>>(commands[connection.info.flavor]!, {
        host: connection.host,
        change,
      });
      if ("Ok" in settings) saved(settings.Ok);

      const unread = "Err" in settings ? ` The settings couldn't be read again: ${reason(settings.Err)}` : "";
      say((error !== null ? reason(error) : done) + unread, error !== null || unread !== "");

      return error === null;
    } catch (error) {
      say(reason(error), true);
      return false;
    } finally {
      isSaving = false;
    }
  }

  /** What the settings saved, which they then show. The hardware, its FRU among it, reads afresh, and what is turned
   * off goes (Rust ejects the image of AMT itself). */
  function saved(data: unknown) {
    pages.set(names.at(-1)!, { data, read: new Date() });
    names.filter((name) => tabs[name].isFixed).forEach((name) => pages.delete(name));
    landed(data);

    const { is_kvm, is_sol, is_ider } = connection.info.remote_access;
    if (is_kvm !== true) close("kvm");
    if (!is_sol) close("sol");
    if (isAmt && !is_ider) image = undefined;
  }

  /** What the settings, read or saved, say of the device: those of AMT its name, and what it lets us redirect. */
  function landed(data: unknown) {
    const { general, remote_access } = data as Partial<SettingsData>;
    if (general !== undefined) connection.info.name = general.name;
    if (remote_access !== undefined) connection.info.remote_access = remote_access;
  }

  /** Why the screen, serial console or a disk image cannot open, if it cannot. */
  function off(kind: "kvm" | "sol" | "ider") {
    const access = connection.info.remote_access;
    if (kind === "kvm" && access.is_kvm === null) return "This device has no KVM.";
    if (kind === "sol" && connection.info.flavor === "Redfish") return "A BMC's serial console is IPMI's, which this one has off.";
    if (access[`is_${kind}`] === true) return;

    const subject = { kvm: "KVM is", sol: "The serial console is", ider: "Disk images are" }[kind];
    const place =
      isAmt ? "Settings under Remote access" : kind === "sol" ? "the BMC's own web pages" : "Settings under Services";
    return `${subject} turned off, in ${place}.`;
  }

  /** Reads the tab afresh. It starts blank so that a refresh shows it happened, unless `quietly` (the next read of a
   * tab that has `everyMs`). Only the last read lands: one whose page is still the one it found, `before`. */
  async function read(name = tab, quietly = false) {
    const before = quietly ? pages.get(name) : {};
    if (!quietly) pages.set(name, before!);

    const page = await invoke(name, { host: connection.host }).then(
      (data) => ({ data, read: new Date() }),
      // One quiet read that fails leaves what the one before read.
      (error) => (quietly ? before : { error: reason(error) }),
    );
    if (pages.get(name) !== before || !page) return;

    pages.set(name, page);
    if (name === names.at(-1) && page.data) landed(page.data);
  }

  // A tab with `everyMs` is read again while it is shown, that long after the read before landed, so that a slow device
  // is not asked again before it answered.
  $effect(() => {
    const [name, everyMs] = [tab, tabs[tab]?.everyMs];
    if (hidden || everyMs === undefined) return;

    let isShown = true;
    (async () => {
      while (isShown) {
        await new Promise((resume) => setTimeout(resume, everyMs));
        if (isShown) await read(name, true);
      }
    })();

    return () => (isShown = false);
  });

  let dialogElement: HTMLDialogElement;
  let asking = $state({ question: "", answers: [] as string[], isSafe: false });

  /** Asks `question` in a dialog, with Cancel and `answers`, the last of which is the one to think twice about unless
   * `isSafe`. Says the answer, or nothing for Cancel. */
  function ask(question: string, answers: string[], isSafe = false) {
    asking = { question, answers, isSafe };
    dialogElement.returnValue = "";
    dialogElement.showModal();

    return new Promise<string>((resolve) =>
      dialogElement.addEventListener("close", () => resolve(dialogElement.returnValue), { once: true }),
    );
  }

  let consentDialogElement: HTMLDialogElement;

  /** What consent is asked for, and how sending the code goes. */
  let consenting = $state({ purpose: "", error: "", isSending: false });

  /** Consent being asked for, which whoever needs it meanwhile waits for too, and how it is answered. */
  let consented: Promise<boolean> | undefined;
  let answer = (_: boolean) => {};

  /** Asks someone at the device for consent: it shows a code on its screen, which they read out. */
  function consent(kind: "kvm" | "sol" | "ider") {
    const purpose = { kvm: "Seeing its screen", sol: "Its serial console", ider: "Putting a disk image in its drive" }[kind];

    consented ??= (async () => {
      try {
        await invoke("consent_start", { host: connection.host });
      } catch (error) {
        say(reason(error), true);
        return false;
      }

      consenting = { purpose, error: "", isSending: false };
      consentDialogElement.querySelector("form")!.reset();
      consentDialogElement.showModal();

      return new Promise<boolean>((resolve) => (answer = resolve));
    })().finally(() => (consented = undefined));

    return consented;
  }

  async function sendCode(event: SubmitEvent) {
    event.preventDefault();

    // We take the code however it is typed: "123 456", say.
    const digits = String(new FormData(event.currentTarget as HTMLFormElement).get("code")).replace(/\D/g, "");
    if (digits.length !== 6) return (consenting.error = "The code has 6 digits.");

    consenting = { ...consenting, error: "", isSending: true };
    try {
      await invoke("consent_send", { host: connection.host, code: Number(digits) });
      consentDialogElement.close();
      say("Consent given.");
      answer(true);
    } catch (error) {
      consenting = { ...consenting, error: reason(error), isSending: false };
    }
  }

  /** The code goes off the device's screen too. */
  function decline() {
    invoke("consent_cancel", { host: connection.host }).catch(() => {});
    answer(false);
  }

  /** Its power, which we read every 15 seconds, shown or not, so that changes from elsewhere (the OS shutting down,
   * another console) show up in the sidebar too. We also read it at once when the device is shown and when the power
   * menu opens, which offers what the device can do right now. We do one read at a time, as a device that does not
   * answer takes a while to say so. It may not answer for a while (AMT can be busy while it resets), so only after two
   * minutes is it `lost` and until then its power is unknown. */
  let isReadingPower = false;
  async function readPower() {
    if (isReadingPower) return;

    isReadingPower = true;
    try {
      connection.power = await invoke<Power>("power", { host: connection.host });
      connection.silentSince = undefined;

      const changing = connection.changing;
      if (changing !== undefined && (connection.power.state !== changing.from || Date.now() > changing.until)) {
        connection.changing = undefined;
      }
    } catch (error) {
      if ((error as CommandError).kind !== "Unreachable") {
        if (!hidden) say(reason(error), true);
      } else if (Date.now() - (connection.silentSince ??= Date.now()) > 120_000) {
        lost(error as CommandError);
      } else {
        connection.power = undefined;
      }
    } finally {
      isReadingPower = false;
    }
  }
  $effect(() => {
    if (!hidden) untrack(readPower);
  });
  $effect(() => {
    const every = setInterval(readPower, 15_000);
    return () => clearInterval(every);
  });

  let asks = $derived(isAmt ? ", while AMT can ask it" : "");
  let isActing = $state(false);

  /** Powering on needs no second thought, everything else does. */
  async function choose(action: PowerAction, label: string) {
    if (!action.startsWith("On") && (await ask(`${label} ${title}?`, [label])) === "") return;

    isActing = true;
    try {
      await invoke("set_power", { host: connection.host, action });
      if (connection.power) connection.changing = { from: connection.power.state, until: Date.now() + 60_000 };
      say(`${label} sent.`);
      // The BIOS's screen goes there.
      if (action.endsWith("Serial")) show("sol");
    } catch (error) {
      say(reason(error), true);
    } finally {
      isActing = false;
    }
  }

  /** The disk image in its drive (see `Mounted`), how much of it the device read and how fast (bytes a second), and
   * when it last read each of its `CELLS` stretches (`Date.now()`, 0 for never). */
  let image: (Mounted & { read: number; rate: number; cells: number[] }) | undefined = $state();
  let isMounting = $state(false);

  // The image is gone with us: a disconnect closes its drive, which is then nothing to say.
  $effect(() => () => (image = undefined));

  /** The image is read from here as the device asks for it, so the file never goes through the webview. Rust tells us
   * what of it each read was, and we work out how fast it reads every second while it does. */
  async function mount(path?: string | null) {
    path ??= await invoke<string | null>("pick", { kind: "Disk image", extensions: ["iso"] });
    if (path === null) return;

    isMounting = true;
    const mounted = $state({ id: 0, name: "", size_bytes: 0, read: 0, rate: 0, cells: Array<number>(CELLS).fill(0) });
    let [tick, last] = [0, 0];
    let ended: CommandError | null | undefined;
    const out = (error: CommandError | null) =>
      error !== null
        ? say(`${mounted.name} came out of the drive: ${reason(error)}`, true)
        : say(`${title} closed the disk image (a reset, say), mount it again to use it.`);

    // A BMC reads the image as soon as it is in, so it may come out before `mount` answers, which then says so.
    const drive = new Channel<Drive>((event) => {
      if (event.kind === "Ended") {
        ended = event.error;
        if (image !== mounted) return;

        image = undefined;
        return out(ended);
      }
      if (mounted.size_bytes === 0) return;

      mounted.read += event.end - event.start;
      const cell = (offset: number) => (offset * CELLS) / mounted.size_bytes;
      for (let i = Math.floor(cell(event.start)); i < cell(event.end); i++) mounted.cells[i] = Date.now();

      if (tick !== 0) return;

      tick = setInterval(() => {
        [mounted.rate, last] = [mounted.read - last, mounted.read];
        if (mounted.rate !== 0) return;

        clearInterval(tick);
        tick = 0;
      }, 1000);
    });

    try {
      Object.assign(mounted, await invoke<Mounted>("mount", { host: connection.host, path, drive }));
      if (ended !== undefined) return out(ended);

      image = mounted;
      const seen = isAmt ? "the device sees it from its next boot on" : "a USB CD-ROM drive on the device";
      say(`${mounted.name} is in the drive, ${seen}.`);
    } catch (error) {
      if ((error as CommandError).kind === "Consent" && (await consent("ider"))) return await mount(path);
      say(reason(error), true);
    } finally {
      isMounting = false;
    }
  }

  /** Moves between the tabs as tabs do: the arrow keys to the one before or after, Home and End to the first or
   * last. */
  function arrows(event: KeyboardEvent & { currentTarget: HTMLElement }) {
    const buttons = [...event.currentTarget.querySelectorAll<HTMLElement>("[role=tab]")];
    const index = buttons.indexOf(document.activeElement as HTMLElement);
    const nextIndex = { ArrowLeft: index - 1, ArrowRight: index + 1, Home: 0, End: -1 }[event.key];
    if (index < 0 || nextIndex === undefined) return;

    event.preventDefault();
    const next = buttons.at(nextIndex % buttons.length)!;
    next.focus();
    next.click();
  }
</script>

<!-- ⌘R reads the tab on screen again, App keeps it from reloading the whole app. -->
<svelte:window
  onkeydown={(event) => {
    if (!hidden && isLive && (event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "r") read();
  }}
/>

<!-- A tab that is `off` looks it, and says why when clicked. Only the one shown is in the tab order, the arrow keys
     move between them (see `arrows`). -->
{#snippet tabButton(name: typeof tab, label: string, off?: string)}
  {@const panel = name === "kvm" || name === "sol" ? opened[name] && name : tabs[tab] && "page"}
  <button
    type="button"
    role="tab"
    id="{id}-{name}"
    class="-mb-px h-10 border-b-2 border-transparent text-muted hover:text-ink aria-selected:border-accent aria-selected:text-ink"
    aria-selected={name === tab}
    aria-controls={panel ? `${id}-${panel}-panel` : undefined}
    aria-disabled={off !== undefined}
    tabindex={name === tab ? 0 : -1}
    title={off}
    onclick={() => (off !== undefined ? say(off) : show(name))}
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
      (name.endsWith("Image") && image === undefined) ||
      (name.endsWith("Serial") && off("sol") !== undefined)}
    title={help}
    onclick={() => choose(name, label)}
  >
    {label}
  </button>
{/snippet}

<!-- What the footer says of it while it is on screen: what its screen and serial console wait for, how large its screen
     is, and its disk image as the device reads it: how fast, from red to green at the top speed, and which stretches,
     like the map of a defragmenter. Those fade for as long as reading all of it at the top speed would take, so that a
     whole read leaves a trail across them. -->
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
      {@const speed = Math.min(image.rate / TOP_BYTES_PER_SECOND, 1)}
      <div
        class="flex gap-0.75"
        title="Disk read {(image.rate / 1e6).toFixed(2)} MB/s{isAmt
          ? `, which top out at about ${TOP_BYTES_PER_SECOND / 1e6} MB/s as each read goes through a small management engine`
          : ''}."
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
          style:--decay="{image.size_bytes / TOP_BYTES_PER_SECOND}s"
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
    <h1 class="mr-auto truncate text-xl font-semibold">{title}</h1>
    <!-- The disk image in the drive of the device, for booting from: IDER for AMT, virtual media for a BMC. -->
    {#if image}
      <!-- Lit while the device reads, like a drive's light. -->
      <span class="flex min-w-0 items-center gap-2 text-muted" title="In the device's drive">
        <Disc3 class={["size-4 flex-none", image.rate && "text-accent"]} />
        <span class="truncate">{image.name}</span>
        <button
          type="button"
          class="btn-icon size-7 flex-none"
          title="Eject"
          onclick={() => {
            invoke("eject", { host: connection.host, id: image!.id });
            image = undefined;
          }}
        >
          <X class="size-4" />
        </button>
      </span>
    {:else if isAmt || connection.info.flavor === "Supermicro"}
      <button
        type="button"
        class="btn btn-secondary"
        disabled={isMounting}
        aria-disabled={off("ider") !== undefined}
        title={off("ider")}
        onclick={() => (off("ider") !== undefined ? say(off("ider")!) : mount())}
      >
        {#if isMounting}<LoaderCircle class="size-4 animate-spin" />{:else}<Disc3 class="size-4" />{/if} Disk image…
      </button>
    {/if}
    {#if !isAmt}<Panel host={connection.host} {hidden} {say} />{/if}
    <button type="button" class="btn btn-secondary" popovertarget="{id}-power" disabled={isActing}>
      Power {#if isActing}<LoaderCircle class="size-4 animate-spin" />{:else}<ChevronDown class="size-4" />{/if}
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
      {@render action("SoftReset", "Restart", `Asks the OS to restart${asks}.`)}
      {@render action("SoftOff", "Shut down", `Asks the OS to shut down${asks}.`)}
      {@render action("Sleep", "Sleep", `Asks the OS to sleep${asks}.`)}
      {@render action("Hibernate", "Hibernate", `Asks the OS to hibernate${asks}.`)}
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
    {#each Object.entries(tabs).slice(0, -1) as [name, { label }] (name)}{@render tabButton(name, label)}{/each}
    {@render tabButton("kvm", "KVM", off("kvm"))}
    {@render closeButton("kvm", "Close the screen")}
    {@render tabButton("sol", "Serial", off("sol"))}
    {@render closeButton("sol", "Close the serial console")}
    <!-- The settings come last, as they are looked at least. -->
    {#each Object.entries(tabs).slice(-1) as [name, { label }] (name)}{@render tabButton(name, label)}{/each}
    {#if isLive}
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
  {#if tabs[tab]}
    {@const Page = tabs[tab].page}
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
          <!-- Each page knows the shape of its own data, the ones that change something take the rest too. Read
               afresh it starts afresh, forms and all: a select falls back to what the device says, what was typed
               does not. -->
          {#key !page?.data}
            <Page data={page?.data as never} {connection} {title} {say} {ask} {save} busy={!page?.data || isSaving} />
          {/key}
          <!-- What the settings do not have is in the web pages of the device itself. -->
          {#if tab === names.at(-1)}
            <p class="text-center text-muted">
              {isAmt ? "Users, and what isn't here, are" : "Everything else is"} in
              <button
                type="button"
                class="text-accent hover:underline"
                onclick={() => invoke("web_pages", { host: connection.host }).catch((error) => say(reason(error), true))}
              >
                {isAmt ? "AMT's" : "the BMC's"} own web pages</button
              >.
            </p>
          {/if}
        {/if}
      </div>
    </div>
  {/if}
</section>

<dialog bind:this={dialogElement}>
  <form method="dialog" class="grid gap-5">
    <p class="font-medium">{asking.question}</p>
    <div class="flex justify-end gap-2">
      <button value="" class="btn btn-secondary">Cancel</button>
      {#each asking.answers as answer, i (answer)}
        {@const isLast = i === asking.answers.length - 1}
        <button value={answer} class={["btn", !isLast ? "btn-secondary" : asking.isSafe ? "btn-primary" : "btn-danger"]}>
          {answer}
        </button>
      {/each}
    </div>
  </form>
</dialog>

<!-- Escape is a no, like Cancel, but not once the code is on its way: the device may take it. -->
<dialog bind:this={consentDialogElement} oncancel={(event) => (consenting.isSending ? event.preventDefault() : decline())}>
  <form class="grid gap-4" onsubmit={sendCode}>
    <p class="font-medium">Ask someone at {title}</p>
    <p>
      {consenting.purpose} needs the consent of someone at the device. It shows a 6-digit code on its screen now: have them
      read it out to you.
    </p>
    <input
      name="code"
      class="field w-full text-center text-lg tracking-[0.5em] tabular-nums"
      inputmode="numeric"
      autocomplete="off"
      aria-label="Code"
      placeholder="000000"
      required
    />
    {#if consenting.error}<p class="text-danger">{consenting.error}</p>{/if}
    <div class="flex justify-end gap-2">
      <button
        type="button"
        class="btn btn-secondary"
        disabled={consenting.isSending}
        onclick={() => (consentDialogElement.close(), decline())}
      >
        Cancel
      </button>
      <button class="btn btn-primary" disabled={consenting.isSending}>
        {#if consenting.isSending}<LoaderCircle class="size-4 animate-spin" />{/if} Send
      </button>
    </div>
  </form>
</dialog>
