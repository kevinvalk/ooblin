<!-- The device's screen, keyboard and mouse. Rust decodes, we paint what changed and send what's pressed: keys by
     where they sit, the mouse in the screen's own pixels. Full screen fills the screen with this alone. A screen that
     goes away (AMT drops it while the device boots, say) keeps its last picture until it's back. Hidden, it stays open
     and painted. -->
<script lang="ts">
  import { ChevronDown, LoaderCircle, Maximize, Minimize } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { CommandError } from "../bindings/console";
  import type { Input } from "../bindings/core";

  let { host, hidden }: { host: string; hidden: boolean } = $props();

  let canvas: HTMLCanvasElement;
  let painted = $state(false);
  /** Why the screen went away, until it's back. */
  let error = $state("");
  let attempt = $state(0);
  /** Failures in a row: we wait longer after each, up to half a minute, so a device that's busy isn't kept busier. */
  let failures = 0;
  /** The last session closing: the next opens once it's gone, AMT takes one at a time. */
  let closing: Promise<unknown> = Promise.resolve();
  let fullscreen = $state(false);

  // The screen is painted until we leave, each change asked for once the one before is painted. When it fails we try
  // again a little later.
  $effect(() => {
    attempt;
    const target = host;
    const context = canvas.getContext("2d")!;
    let open = true;
    let retry = 0;
    painted = false;
    (async () => {
      try {
        await closing;
        if (!open) return;
        await invoke("kvm_open", { host: target });
        canvas.focus();
        while (open) {
          paint(context, await invoke<ArrayBuffer>("kvm_frame", { host: target }));
          painted = true;
          error = "";
          failures = 0;
        }
      } catch (failure) {
        if (!open) return;
        painted = false;
        error = (failure as CommandError).message ?? String(failure);
        retry = setTimeout(() => attempt++, Math.min(3000 * 2 ** failures++, 30_000));
      }
    })();
    return () => {
      open = false;
      clearTimeout(retry);
      closing = invoke("kvm_close", { host: target });
    };
  });

  /** A frame is the screen's width and height, then per change its x, y, width, height and RGBA pixels. */
  function paint(context: CanvasRenderingContext2D, frame: ArrayBuffer) {
    const view = new DataView(frame);
    const u16 = (at: number) => view.getUint16(at, true);
    if (canvas.width !== u16(0) || canvas.height !== u16(2)) [canvas.width, canvas.height] = [u16(0), u16(2)];
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
    for (const input of inputs) {
      const last = waiting.at(-1);
      const moved = input.kind === "Pointer" && last?.kind === "Pointer" && last.buttons === input.buttons;
      if (moved && !last.scroll && !input.scroll) waiting[waiting.length - 1] = input;
      else waiting.push(input);
    }
    if (sending) return;
    sending = true;
    // A screen that's gone says so in its frames.
    while (waiting.length) await invoke("kvm_input", { host, inputs: waiting.splice(0) }).catch(() => {});
    sending = false;
  }

  const mac = navigator.userAgent.includes("Mac");

  function key(event: KeyboardEvent, down: boolean) {
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

  /** Where the pointer is on the screen, which keeps its proportions within the canvas (`object-contain`). */
  function pointer(event: MouseEvent, scroll = 0) {
    const box = canvas.getBoundingClientRect();
    const scale = Math.min(box.width / canvas.width, box.height / canvas.height);
    const at = (offset: number, size: number, pixels: number) =>
      Math.min(Math.max(Math.floor((offset - (size - pixels * scale) / 2) / scale), 0), pixels - 1);
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

  let typing: HTMLDialogElement;
  let text = $state("");

  function toggleFullscreen() {
    fullscreen = !fullscreen;
    invoke("fullscreen", { on: fullscreen });
    canvas.focus();
  }

  // Leaving the tab leaves full screen.
  $effect(() => () => {
    if (fullscreen) invoke("fullscreen", { on: false });
  });
</script>

<main class={["grid min-h-0 grid-rows-[auto_1fr]", fullscreen ? "fixed inset-0 z-50 bg-canvas" : "px-8 pb-8"]} {hidden}>
  <div class={["flex items-center gap-2", fullscreen ? "px-3 py-2" : "pt-4 pb-3"]}>
    {#if !painted}
      <LoaderCircle class="size-4 flex-none animate-spin text-muted" />
      <span class="truncate text-muted" title={error}>{error ? `Reconnecting, ${error}` : "Opening the screen…"}</span>
    {/if}
    <!-- A native menu, looking like our buttons. -->
    <label class="btn btn-secondary relative ml-auto h-7 px-2.5">
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
    <button type="button" class="btn btn-secondary h-7 px-2.5" onclick={() => typing.showModal()}>Type text…</button>
    <button type="button" class="btn-icon size-7" title={fullscreen ? "Exit full screen" : "Full screen"} onclick={toggleFullscreen}>
      {#if fullscreen}<Minimize class="size-4" />{:else}<Maximize class="size-4" />{/if}
    </button>
  </div>

  <!-- The device's own screen, so its own pointer: ours stays visible on top, as it moves without delay. -->
  <canvas
    bind:this={canvas}
    class="size-full min-h-0 bg-black object-contain outline-none"
    tabindex="0"
    onkeydown={(event) => key(event, true)}
    onkeyup={(event) => key(event, false)}
    onblur={() => send({ kind: "Release" })}
    onpointerdown={(event) => {
      canvas.setPointerCapture(event.pointerId);
      pointer(event);
    }}
    onpointermove={(event) => pointer(event)}
    onpointerup={(event) => pointer(event)}
    onwheel={wheel}
    oncontextmenu={(event) => event.preventDefault()}
  ></canvas>
</main>

<dialog
  bind:this={typing}
  class="dialog w-96"
  onclose={() => {
    if (typing.returnValue === "type") send({ kind: "Text", text });
    text = "";
    canvas.focus();
  }}
>
  <form method="dialog" class="grid gap-5">
    <p class="font-medium">Type text on the device</p>
    <textarea class="field h-32 py-2" bind:value={text} placeholder="A US keyboard's letters, digits and symbols"></textarea>
    <div class="flex justify-end gap-2">
      <button value="cancel" class="btn btn-secondary">Cancel</button>
      <button value="type" class="btn btn-primary" disabled={!text}>Type</button>
    </div>
  </form>
</dialog>
