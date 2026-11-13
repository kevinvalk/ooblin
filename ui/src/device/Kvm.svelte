<!-- The device's screen, keyboard and mouse. Rust decodes, we paint what changed and send what's pressed: keys by
     where they sit, the mouse in the screen's own pixels. Full screen fills the screen with this alone. A screen that
     goes away (AMT drops it while the device boots, say) keeps its last picture until it's back, the footer saying so
     meanwhile. Hidden, it stays open and painted, and lets go of what's held. Without consent, or ended by the device on
     purpose, it stops, until connected again. -->
<script lang="ts">
  import { ChevronDown, Maximize, Minimize, Scaling, SendHorizontal } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import type { Input, Screens } from "../bindings/core";
  import { reason } from "./Device.svelte";
  import Redirection from "./Redirection.svelte";

  let {
    id: panel,
    host,
    hidden,
    consent,
    status,
    say,
    measured,
  }: {
    id: string;
    host: string;
    hidden: boolean;
    consent: () => Promise<boolean>;
    status: (text: string) => void;
    say: (text: string, failed?: boolean) => void;
    measured: (screen?: { width: number; height: number; fps: number }) => void;
  } = $props();

  let canvas: HTMLCanvasElement;
  /** How wide the screen is, and how wide the canvas shows it in CSS pixels. */
  let width = $state(0);
  let shown = $state(0);
  let fullscreen = $state(false);
  /** The device's screens, to pick one of where there are several. */
  let screens = $state<Screens>();

  /** The open session's id, which what's pressed goes to. */
  let id: number | undefined;

  /** The screen is painted while it's open, each change asked for once the one before is painted. What was pressed
   * for the one before goes nowhere. */
  async function read(opened: number, open: () => boolean) {
    [id, waiting] = [opened, []];
    const context = canvas.getContext("2d")!;
    if (!hidden) canvas.focus();
    invoke<Screens>("screens", { host }).then((found) => (screens = found), () => {});
    while (open()) paint(context, await invoke<ArrayBuffer>("kvm_frame", { host, id: opened }));
  }

  /** Updates painted this second, said each second with the screen's size, for the footer while it's shown: a screen
   * that doesn't change has none. */
  let painted = 0;
  $effect(() => {
    const counting = setInterval(() => {
      measured(hidden || !canvas.width ? undefined : { width: canvas.width, height: canvas.height, fps: painted });
      painted = 0;
    }, 1000);
    return () => (clearInterval(counting), measured());
  });

  /** A frame is the screen's width and height, then per change its x, y, width, height and RGBA pixels. */
  function paint(context: CanvasRenderingContext2D, frame: ArrayBuffer) {
    const view = new DataView(frame);
    const u16 = (at: number) => view.getUint16(at, true);
    if (canvas.width !== u16(0) || canvas.height !== u16(2)) [width, canvas.width, canvas.height] = [u16(0), u16(0), u16(2)];
    if (frame.byteLength > 4) painted++;
    for (let at = 4; at < frame.byteLength; ) {
      const [x, y, width, height] = [u16(at), u16(at + 2), u16(at + 4), u16(at + 6)];
      const pixels = new Uint8ClampedArray(frame, at + 8, width * height * 4);
      context.putImageData(new ImageData(pixels, width, height), x, y);
      at += 8 + pixels.length;
    }
  }

  /** Inputs wait while a batch is on its way, a mouse move replacing the move before it. */
  let waiting: Input[] = [];
  let sending = false;
  async function send(...inputs: Input[]) {
    if (id === undefined) return;
    for (const input of inputs) {
      const last = waiting.at(-1);
      const moved = input.kind === "Pointer" && last?.kind === "Pointer" && last.buttons === input.buttons;
      if (moved && !last.scroll && !input.scroll) waiting[waiting.length - 1] = input;
      else waiting.push(input);
    }
    if (sending) return;
    sending = true;
    // A screen that's gone says so in its frames.
    while (waiting.length) await invoke("kvm_input", { host, id, inputs: waiting.splice(0) }).catch(() => {});
    sending = false;
  }

  // Hidden, keys and buttons held go nowhere: we let go of them.
  $effect(() => {
    if (hidden) send({ kind: "Release" });
  });

  const mac = navigator.userAgent.includes("Mac");
  const enter = mac ? "⌘Enter" : "Ctrl+Enter";

  /** What's to be typed on the device, and typing it, pressing Enter after it too if asked. */
  let text = $state("");
  function type(pressEnter = false) {
    if (text || pressEnter) send({ kind: "Text", text: pressEnter ? `${text}\n` : text });
    text = "";
  }

  function key(event: KeyboardEvent) {
    const down = event.type === "keydown";
    event.preventDefault();
    event.stopPropagation();
    // The device repeats a held key itself.
    if (event.repeat) return;
    // ⌘ is Ctrl, as a Mac's shortcuts are a PC's. WebKit never says a key held with ⌘ let go, so we let go now.
    const code = mac ? event.code.replace("Meta", "Control") : event.code;
    send({ kind: "Key", code, down });
    if (mac && down && event.metaKey && !/^(Shift|Control|Alt|Meta)/.test(event.code)) {
      send({ kind: "Key", code, down: false });
    }
  }

  /** Where the pointer is on the screen, which the canvas shows whole, maybe shrunk. */
  function pointer(event: MouseEvent, scroll = 0) {
    const box = canvas.getBoundingClientRect();
    const at = (offset: number, size: number, pixels: number) =>
      Math.min(Math.max(Math.floor((offset / size) * pixels), 0), pixels - 1);
    const x = at(event.clientX - box.left, box.width, canvas.width);
    const y = at(event.clientY - box.top, box.height, canvas.height);
    send({ kind: "Pointer", x, y, buttons: event.buttons, scroll });
  }

  /** A trackpad scrolls in many small steps, a notch of the wheel is about 50 pixels. */
  let scrolled = 0;
  function wheel(event: WheelEvent) {
    event.preventDefault();
    scrolled += event.deltaY;
    for (; Math.abs(scrolled) >= 50; scrolled -= Math.sign(scrolled) * 50) pointer(event, Math.sign(scrolled));
  }

  /** Presses the keys in turn, then lets go in reverse. */
  function press(codes: string[]) {
    send(...codes.map((code) => ({ kind: "Key", code, down: true }) as const));
    send(...[...codes].reverse().map((code) => ({ kind: "Key", code, down: false }) as const));
    canvas.focus();
  }

  /** Sizes the window so the screen shows at its own size, a point to a pixel: by as much as its box is off. */
  async function fit() {
    const box = canvas.parentElement!;
    await invoke("grow", { width: canvas.width - box.clientWidth, height: canvas.height - box.clientHeight });
    canvas.focus();
  }

  function toggleFullscreen() {
    fullscreen = !fullscreen;
    invoke("fullscreen", { on: fullscreen });
    canvas.focus();
  }

  // Closing the screen leaves full screen.
  $effect(() => () => {
    if (fullscreen) invoke("fullscreen", { on: false });
  });
</script>

<!-- The window may leave full screen without us, the OS's own way. -->
<svelte:window onresize={() => fullscreen && getCurrentWindow().isFullscreen().then((on) => (fullscreen = on))} />

<div
  id={panel}
  class={["grid min-h-0 grid-rows-[auto_1fr]", fullscreen ? "fixed inset-0 z-50 bg-canvas" : "px-8 pb-8"]}
  role="tabpanel"
  aria-label="KVM"
  {hidden}
>
  <div class={["flex items-start gap-3", fullscreen ? "px-3 py-2" : "pt-4 pb-3"]}>
    <!-- Text to type on the device as a US keyboard would, a line break as Enter: it grows with what's typed or pasted
         into it, up to six lines. Enter types it, Shift+Enter starts a new line in it first. -->
    <form
      class="field flex h-auto min-h-7 min-w-0 flex-1 items-start gap-1 py-1 pr-1 pl-2.5"
      onsubmit={(event) => {
        event.preventDefault();
        type();
      }}
    >
      <textarea
        bind:value={text}
        rows={Math.min(text.split("\n").length, 6)}
        class="resize-none leading-5"
        autocapitalize="off"
        spellcheck="false"
        autocomplete="off"
        placeholder="Text to type on the device"
        title="Typed as a US keyboard would: its letters, digits and symbols, each line break as Enter. Shift+Enter starts a new line, {enter} presses Enter after it too."
        onkeydown={(event) => {
          if (event.key !== "Enter" || event.shiftKey || event.isComposing) return;
          event.preventDefault();
          type(event.metaKey || event.ctrlKey);
        }}
      ></textarea>
      <button class="btn-icon size-5 flex-none" title="Type it (Enter), then press Enter too ({enter})">
        <SendHorizontal class="size-3.5" />
      </button>
    </form>
    <!-- Numbered like the OS numbers them, from 1, a choice once AMT says more than one is active. -->
    {#if screens}
      <select
        class="field h-7 px-2"
        disabled={screens.active.length < 2}
        title="The device's screen to show"
        onchange={async (event) => {
          const select = event.currentTarget;
          try {
            await invoke("show_screen", { host, screen: Number(select.value) });
            screens!.shown = Number(select.value);
          } catch (error) {
            say(reason(error), true);
            select.value = String(screens!.shown);
          }
        }}
      >
        {#each screens.active.length ? screens.active : [screens.shown] as screen (screen)}
          <option value={screen} selected={screen === screens.shown}>Screen {screen + 1}</option>
        {/each}
      </select>
    {/if}
    <!-- A native menu, looking like our buttons. -->
    <label class="btn btn-secondary relative h-7 px-2.5">
      Send keys <ChevronDown class="size-4" />
      <select
        class="absolute inset-0 opacity-0"
        value=""
        onchange={(event) => {
          press(event.currentTarget.value.split(" "));
          event.currentTarget.value = "";
        }}
      >
        <option value="" hidden></option>
        <option value="ControlLeft AltLeft Delete">Ctrl+Alt+Del</option>
        <option value="AltLeft Tab">Alt+Tab</option>
        <option value="AltLeft F4">Alt+F4</option>
        <option value="MetaLeft">Windows</option>
        <option value="MetaLeft KeyL">Windows+L</option>
        <option value="MetaLeft KeyR">Windows+R</option>
        <option value="ControlLeft AltLeft F1">Ctrl+Alt+F1</option>
        <option value="ControlLeft AltLeft F2">Ctrl+Alt+F2</option>
        <option value="PrintScreen">Print Screen</option>
      </select>
    </label>
    {#if !fullscreen}
      <button
        type="button"
        class="btn-icon size-7"
        disabled={!width}
        title="Size the window to the screen, at its own size: a point to each of its pixels"
        onclick={fit}
      >
        <Scaling class="size-4" />
      </button>
    {/if}
    <button type="button" class="btn-icon size-7" title={fullscreen ? "Exit full screen" : "Full screen"} onclick={toggleFullscreen}>
      {#if fullscreen}<Minimize class="size-4" />{:else}<Maximize class="size-4" />{/if}
    </button>
  </div>

  <!-- The device's own screen, so its own pointer: ours stays visible on top, as it moves without delay. At its own
       size on black, shrunk only to fit, and its pixels kept sharp (not smoothed onto a Retina display's) unless it is.
       Ringed while it has the keys, every one of which goes to the device: a click outside takes them back. -->
  <Redirection kind="kvm" {host} {consent} {status} {read}>
    <!-- An application to screen readers, which then leave its keys to it. -->
    <!-- svelte-ignore a11y_no_interactive_element_to_noninteractive_role -->
    <canvas
      bind:this={canvas}
      bind:clientWidth={shown}
      class={[
        "max-h-full max-w-full outline-none focus:outline-2 focus:-outline-offset-2 focus:outline-accent",
        shown >= width && "[image-rendering:pixelated]",
      ]}
      role="application"
      aria-label="The device's screen"
      title="Every key goes to the device while it has focus, a click outside it takes them back."
      tabindex="0"
      onkeydown={key}
      onkeyup={key}
      onblur={() => send({ kind: "Release" })}
      onpointerdown={(event) => (canvas.setPointerCapture(event.pointerId), pointer(event))}
      onpointermove={pointer}
      onpointerup={pointer}
      onpointercancel={() => send({ kind: "Release" })}
      onwheel={wheel}
      oncontextmenu={(event) => event.preventDefault()}
    ></canvas>
  </Redirection>
</div>
