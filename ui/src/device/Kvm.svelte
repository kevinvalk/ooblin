<script lang="ts">
  import { ChevronDown, Maximize, Minimize, Scaling, SendHorizontal } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import type { Input, Screens } from "../bindings/core";
  import { reason } from "../connection";
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
    say: (text: string, isFailed?: boolean) => void;
    measured: (screen?: { width: number; height: number; fps: number }) => void;
  } = $props();

  let canvasElement: HTMLCanvasElement;
  let width = $state(0); // How wide the screen is
  let shownWidth = $state(0); // How wide the canvas shows it, in CSS pixels
  let isFullscreen = $state(false);

  /** The screens of the device, to pick one of where there are several. */
  let screens = $state<Screens>();

  /** The id of the open session, which what is pressed goes to. */
  let id: number | undefined;

  /** Rust decodes the screen, we paint what changed while it is open and ask for each change once the one before is
   * painted. A screen that goes away (AMT drops it while the device boots, say) keeps its last picture until it is
   * back, and what was pressed for the one before goes nowhere. */
  async function read(opened: number, isOpen: () => boolean) {
    [id, waiting] = [opened, []];
    const context = canvasElement.getContext("2d")!;
    if (!hidden) canvasElement.focus();

    invoke<Screens>("screens", { host }).then((found) => (screens = found), () => {});
    while (isOpen()) paint(context, await invoke<ArrayBuffer>("kvm_frame", { host, id: opened }));
  }

  /** The updates painted this second, which we say each second with the size of the screen, for the footer while it is
   * shown. A screen that does not change has none. */
  let painted = 0;
  $effect(() => {
    const counting = setInterval(() => {
      // We do not take the width of the canvas itself, which is 300 before its first frame.
      measured(hidden || width === 0 ? undefined : { width, height: canvasElement.height, fps: painted });
      painted = 0;
    }, 1000);
    return () => (clearInterval(counting), measured());
  });

  /** A frame is the screen's width and height, then per change its x, y, width, height and RGBA pixels. */
  function paint(context: CanvasRenderingContext2D, frame: ArrayBuffer) {
    const view = new DataView(frame);
    const u16 = (offset: number) => view.getUint16(offset, true);

    if (canvasElement.width !== u16(0) || canvasElement.height !== u16(2)) {
      [width, canvasElement.width, canvasElement.height] = [u16(0), u16(0), u16(2)];
    }
    if (frame.byteLength > 4) painted++;

    for (let offset = 4; offset < frame.byteLength; ) {
      const [x, y, width, height] = [u16(offset), u16(offset + 2), u16(offset + 4), u16(offset + 6)];
      const pixels = new Uint8ClampedArray(frame, offset + 8, width * height * 4);
      context.putImageData(new ImageData(pixels, width, height), x, y);
      offset += 8 + pixels.length;
    }
  }

  /** Inputs wait while a batch is on its way, a mouse move replacing the move before it. */
  let waiting: Input[] = [];
  let isSending = false;
  async function send(...inputs: Input[]) {
    if (id === undefined) return;

    for (const input of inputs) {
      const last = waiting.at(-1);
      const isMove = input.kind === "Pointer" && last?.kind === "Pointer" && last.buttons === input.buttons;
      if (isMove && last.scroll === 0 && input.scroll === 0) waiting[waiting.length - 1] = input;
      else waiting.push(input);
    }
    if (isSending) return;

    isSending = true;
    // A screen that is gone says so in its frames.
    while (waiting.length > 0) await invoke("kvm_input", { host, id, inputs: waiting.splice(0) }).catch(() => {});
    isSending = false;
  }

  // Hidden, keys and buttons held go nowhere, so we let go of them.
  $effect(() => {
    if (hidden) send({ kind: "Release" });
  });

  const isMac = navigator.userAgent.includes("Mac");
  const enter = isMac ? "⌘Enter" : "Ctrl+Enter";

  /** What is to be typed on the device, and typing it, pressing Enter after it too if asked. */
  let text = $state("");
  function type(pressEnter = false) {
    if (text !== "" || pressEnter) send({ kind: "Text", text: pressEnter ? `${text}\n` : text });
    text = "";
  }

  function key(event: KeyboardEvent) {
    const isDown = event.type === "keydown";
    event.preventDefault();
    event.stopPropagation();
    if (event.repeat) return; // The device repeats a held key itself

    // ⌘ is Ctrl, as the shortcuts of a Mac are those of a PC. WebKit never says a key held with ⌘ let go, so we let go
    // now.
    const code = isMac ? event.code.replace("Meta", "Control") : event.code;
    send({ kind: "Key", code, is_down: isDown });
    if (isMac && isDown && event.metaKey && !/^(Shift|Control|Alt|Meta)/.test(event.code)) {
      send({ kind: "Key", code, is_down: false });
    }
  }

  /** Where the pointer is on the screen, which the canvas shows whole, maybe shrunk. */
  function pointer(event: MouseEvent, scroll = 0) {
    const box = canvasElement.getBoundingClientRect();
    const position = (offset: number, size: number, pixels: number) =>
      Math.min(Math.max(Math.floor((offset / size) * pixels), 0), pixels - 1);

    const x = position(event.clientX - box.left, box.width, canvasElement.width);
    const y = position(event.clientY - box.top, box.height, canvasElement.height);
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
    send(...codes.map((code) => ({ kind: "Key", code, is_down: true }) as const));
    send(...[...codes].reverse().map((code) => ({ kind: "Key", code, is_down: false }) as const));
    canvasElement.focus();
  }

  /** Sizes the window so that the screen shows at its own size, a point to a pixel: by as much as its box is off. */
  async function fit() {
    const boxElement = canvasElement.parentElement!;
    await invoke("grow", {
      width: canvasElement.width - boxElement.clientWidth,
      height: canvasElement.height - boxElement.clientHeight,
    });
    canvasElement.focus();
  }

  // Closing the screen leaves full screen.
  $effect(() => () => {
    if (isFullscreen) invoke("fullscreen", { isOn: false });
  });
</script>

<!-- The window may leave full screen without us, the way of the OS itself. -->
<svelte:window onresize={() => isFullscreen && getCurrentWindow().isFullscreen().then((on) => (isFullscreen = on))} />

<div
  id={panel}
  class={["grid min-h-0 grid-rows-[auto_1fr]", isFullscreen ? "fixed inset-0 z-50 bg-canvas" : "px-8 pb-8"]}
  role="tabpanel"
  aria-label="KVM"
  {hidden}
>
  <div class={["flex items-start gap-3", isFullscreen ? "px-3 py-2" : "pt-4 pb-3"]}>
    <!-- Text to type on the device as a US keyboard would, a line break as Enter: it grows with what is typed or pasted
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
        {#each screens.active.length > 0 ? screens.active : [screens.shown] as screen (screen)}
          <option value={screen} selected={screen === screens.shown}>Screen {screen + 1}</option>
        {/each}
      </select>
    {/if}
    <!-- A native menu, looking like our buttons. It says where the keys go, as a tooltip on the screen would cover
         it. -->
    <label
      class="btn btn-secondary relative h-7 px-2.5"
      title="Keys this computer keeps to itself. Every other key goes to the device while its screen has focus, a click outside it takes them back."
    >
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
    {#if !isFullscreen}
      <button
        type="button"
        class="btn-icon size-7"
        disabled={width === 0}
        title="Size the window to the screen, at its own size: a point to each of its pixels"
        onclick={fit}
      >
        <Scaling class="size-4" />
      </button>
    {/if}
    <button
      type="button"
      class="btn-icon size-7"
      title={isFullscreen ? "Exit full screen" : "Full screen"}
      onclick={() => {
        isFullscreen = !isFullscreen;
        invoke("fullscreen", { isOn: isFullscreen });
        canvasElement.focus();
      }}
    >
      {#if isFullscreen}<Minimize class="size-4" />{:else}<Maximize class="size-4" />{/if}
    </button>
  </div>

  <!-- The screen of the device itself, so with its own pointer: ours stays visible on top, as it moves without delay.
       It shows at its own size on black, shrunk only to fit, with its pixels kept sharp (not smoothed onto those of a
       Retina display) unless it is shrunk. It is ringed while it has the keys, every one of which goes to the device: a
       click outside takes them back. -->
  <Redirection kind="kvm" {host} {consent} {status} {read}>
    <!-- An application to screen readers, which then leave its keys to it. -->
    <!-- svelte-ignore a11y_no_interactive_element_to_noninteractive_role -->
    <canvas
      bind:this={canvasElement}
      bind:clientWidth={shownWidth}
      class={[
        "max-h-full max-w-full outline-none focus:outline-2 focus:-outline-offset-2 focus:outline-accent",
        shownWidth >= width && "[image-rendering:pixelated]",
      ]}
      role="application"
      aria-label="The device's screen"
      tabindex="0"
      onkeydown={key}
      onkeyup={key}
      onblur={() => send({ kind: "Release" })}
      onpointerdown={(event) => (canvasElement.setPointerCapture(event.pointerId), pointer(event))}
      onpointermove={pointer}
      onpointerup={pointer}
      onpointercancel={() => send({ kind: "Release" })}
      onwheel={wheel}
      oncontextmenu={(event) => event.preventDefault()}
    ></canvas>
  </Redirection>
</div>
