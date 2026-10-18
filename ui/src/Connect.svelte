<!-- The connect form, for `device` if given: a device with a saved password needs none typed. A failed attempt leaves
     everything in place to fix and retry, a pin mismatch shows both fingerprints and a way to trust the new one. -->
<script lang="ts">
  import { LoaderCircle } from "@lucide/svelte";
  import { untrack } from "svelte";
  import type { CommandError, Device } from "./bindings/console";

  type Login = { host: string; user: string; password: string; remember: boolean };
  let {
    device,
    connecting,
    error,
    onsubmit,
    ontrust,
  }: {
    device?: Device;
    connecting: boolean;
    error?: CommandError;
    onsubmit: (login: Login) => void;
    ontrust: (host: string, fingerprint: string) => Promise<unknown>;
  } = $props();

  // The form starts from the device and goes its own way, App gives us a fresh one for another device.
  let login: Login = $state(untrack(() => ({ host: device?.host ?? "", user: device?.user ?? "admin", password: "", remember: false })));
  let saved = $derived(device?.remembered && login.host === device.host);
</script>

<form
  class="mx-auto mt-[12vh] grid w-full max-w-sm content-start gap-4 px-6"
  autocomplete="off"
  spellcheck="false"
  inert={connecting}
  onsubmit={(event) => {
    event.preventDefault();
    onsubmit(login);
  }}
>
  <h1 class="mb-2 text-2xl font-semibold">Connect to a device</h1>
  <!-- svelte-ignore a11y_autofocus -->
  <label class="grid gap-1.5 font-medium">
    Host <input bind:value={login.host} class="field font-normal" placeholder="192.168.1.50" required autofocus={!device} />
  </label>
  <label class="grid gap-1.5 font-medium">
    User <input bind:value={login.user} class="field font-normal" required />
  </label>
  <!-- svelte-ignore a11y_autofocus -->
  <label class="grid gap-1.5 font-medium">
    Password
    <input
      bind:value={login.password}
      class="field font-normal"
      type="password"
      placeholder={saved ? "Saved in the keychain" : ""}
      required={!saved}
      autofocus={!!device && !saved}
    />
  </label>
  <label class="flex items-center gap-2"><input type="checkbox" bind:checked={login.remember} /> Remember the password</label>
  <button class="btn btn-primary mt-2 h-9">
    {#if connecting}<LoaderCircle class="size-4 animate-spin" /> Connecting…{:else}Connect{/if}
  </button>

  {#if error?.kind === "pinMismatch"}
    <div class="grid gap-3">
      <p>
        <strong class="text-danger">The certificate has changed.</strong> That's expected after reprovisioning or
        reflashing the ME, anything else may be someone in the middle.
      </p>
      <!-- 16 bytes a line, so fingerprints can be compared at a glance. -->
      <dl class="grid gap-1 [&_dd]:mb-2 [&_dd]:w-[48ch] [&_dd]:font-mono [&_dd]:text-xs [&_dd]:break-all [&_dd]:select-text">
        <dt class="text-muted">Pinned</dt>
        <dd>{error.expected}</dd>
        <dt class="text-muted">Presented</dt>
        <dd>{error.actual}</dd>
      </dl>
      <button
        type="button"
        class="btn btn-danger justify-self-start"
        onclick={() => ontrust(login.host, error.actual).then(() => onsubmit(login))}
      >
        Trust the new certificate
      </button>
    </div>
  {:else if error}
    <p class="text-danger select-text">{error.message}</p>
  {/if}
</form>
