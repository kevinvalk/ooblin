<!-- The server and its BMC, whose network port is the one we speak to. -->
<script lang="ts">
  import type { BmcStatus } from "../../bindings/core";
  import Network from "../Network.svelte";

  let { data }: { data?: BmcStatus } = $props();

  /** A day as `2026-05-29`, in the reader's own words. */
  const day = (date?: string) => (date ? new Date(`${date}T12:00`).toLocaleDateString() : "");
</script>

<section>
  <h2>Server</h2>
  <dl class="card">
    <div>
      <dt>Power</dt>
      <dd class="flex items-center gap-2">
        {#if data}<span class="dot" data-power={data.power}></span>{data.power}{/if}
      </dd>
    </div>
    <div><dt>BIOS version</dt><dd>{data?.bios}</dd></div>
    <div><dt>BIOS released</dt><dd>{day(data?.bios_released)}</dd></div>
    {#each data?.lans ?? [undefined] as lan (lan?.name)}
      <div><dt>{lan && `${lan.name.replace("OnBoard ", "")} MAC address`}</dt><dd class="font-mono">{lan?.mac}</dd></div>
    {/each}
    <div><dt>System ID</dt><dd class="font-mono">{data?.system_id}</dd></div>
  </dl>
</section>

<section>
  <h2>BMC</h2>
  <dl class="card">
    <div><dt>Name</dt><dd>{data?.info.name}</dd></div>
    <div><dt>Firmware version</dt><dd>{data?.info.version}</dd></div>
    <div><dt>Firmware built</dt><dd>{day(data?.firmware_built)}</dd></div>
    <div><dt>Redfish version</dt><dd>{data?.redfish}</dd></div>
    <div><dt>Certificate</dt><dd class="font-mono text-xs/5">{data?.info.fingerprint}</dd></div>
  </dl>
</section>

<Network ports={data?.ports} />
