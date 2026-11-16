<script module lang="ts">
  import type { Flavor } from "./bindings/core";

  /** `flavor` null is Automatic: the saved one of the device, else what it turns out to be. */
  export type Login = { host: string; user: string; password: string; isRemember: boolean; flavor: Flavor | null };
</script>

<script lang="ts">
  import { LoaderCircle } from "@lucide/svelte";
  import { untrack } from "svelte";
  import type { CommandError, Device } from "./bindings/ooblin";
  import { FLAVORS } from "./connection";

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
    ontrust: (login: Login, fingerprint: string) => void;
  } = $props();

  // The form starts from the device and goes its own way, App gives us a fresh one for another device.
  let login: Login = $state(
    untrack(() => ({ host: device?.host ?? "", user: device?.user ?? "admin", password: "", isRemember: false, flavor: null })),
  );

  // A device with a saved password needs none typed.
  let isSaved = $derived(device?.is_remembered === true && login.host === device.host);
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
    Host <input bind:value={login.host} class="field font-mono font-normal" autocapitalize="off" spellcheck="false" placeholder="192.168.1.50" required autofocus={!device} />
  </label>
  <label class="grid gap-1.5 font-medium" title="How we speak to it. Automatic tries Intel AMT, then a Supermicro's Redfish, then IPMI, then another BMC's Redfish, sending the password only to what answers as one.">
    Kind
    <select bind:value={login.flavor} class="field font-normal">
      <option value={null}>Automatic</option>
      {#each Object.entries(FLAVORS) as [flavor, { title }] (flavor)}<option value={flavor}>{title}</option>{/each}
    </select>
  </label>
  <label class="grid gap-1.5 font-medium">
    User <input bind:value={login.user} class="field font-normal" autocapitalize="off" spellcheck="false" required />
  </label>
  <!-- svelte-ignore a11y_autofocus -->
  <label class="grid gap-1.5 font-medium">
    Password
    <input
      bind:value={login.password}
      class="field font-normal"
      type="password"
      placeholder={isSaved ? "Saved in the keychain" : ""}
      required={!isSaved}
      autofocus={device !== undefined && !isSaved}
    />
  </label>
  {#if !isSaved}
    <label class="flex items-center gap-2"><input type="checkbox" bind:checked={login.isRemember} /> Remember the password</label>
  {/if}
  <button class="btn btn-primary mt-2 h-9">
    {#if connecting}<LoaderCircle class="size-4 animate-spin" /> Connecting…{:else}Connect{/if}
  </button>

  {#if error?.kind === "PinMismatch"}
    <div class="grid gap-3">
      <p>
        <strong class="text-danger">The certificate has changed.</strong> That's expected after the device was
        reprovisioned or its firmware reflashed, anything else may be someone in the middle.
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
        onclick={() => ontrust(login, error.actual)}
      >
        Trust the new certificate
      </button>
    </div>
  {:else if error}
    <p class="text-danger select-text">{error.message}</p>
  {/if}
</form>
