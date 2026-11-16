<script lang="ts">
  import type { BmcStatus } from "../../bindings/core";
  import Network from "../Network.svelte";

  let { data }: { data?: BmcStatus } = $props();

  /** A day as `2026-05-29`, in the words of the reader. */
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
    <!-- The days its BIOS was released and its firmware was built show only where the pages of a vendor say them. -->
    {#if !data || data.bios_released}
      <div><dt>BIOS released</dt><dd>{day(data?.bios_released)}</dd></div>
    {/if}
    {#each data?.lans ?? [undefined] as lan}
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
    {#if !data || data.firmware_built}
      <div><dt>Firmware built</dt><dd>{day(data?.firmware_built)}</dd></div>
    {/if}
    <div><dt>Redfish version</dt><dd>{data?.redfish}</dd></div>
    <div><dt>Certificate</dt><dd class="font-mono text-xs/5">{data?.info.fingerprint}</dd></div>
  </dl>
</section>

<!-- The network port of the BMC is the one we speak to. -->
<Network ports={data?.ports} />
