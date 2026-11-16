<script lang="ts">
  import type { Hardware } from "../../bindings/core";

  // While loading every list shows one of its thing. Several are told apart by number, from 0 like the OS does.
  let { data }: { data?: Hardware } = $props();
</script>

<!-- Redfish says less of the server than AMT does, so there is one System section and Disks only where it lists
     any. -->
<section>
  <h2>System</h2>
  <dl class="card">
    <div><dt>Manufacturer</dt><dd>{data?.system.manufacturer}</dd></div>
    <div><dt>Model</dt><dd>{data?.system.model}</dd></div>
    <div><dt>Serial number</dt><dd>{data?.system.serial_number}</dd></div>
    <div><dt>Board serial number</dt><dd>{data?.motherboard.serial_number}</dd></div>
    <div><dt>Asset tag</dt><dd>{data?.motherboard.asset_tag}</dd></div>
    <div><dt>System ID</dt><dd class="font-mono">{data?.system.id}</dd></div>
    <div><dt>BIOS version</dt><dd>{data?.bios.version}</dd></div>
  </dl>
</section>

<section>
  <h2>Processors</h2>
  <div class="cards">
    {#each data?.processors ?? [undefined] as processor}
      <div>
        <h3>{processor?.socket}</h3>
        <dl class="card">
          <div><dt>Model</dt><dd>{processor?.model}</dd></div>
          <div><dt>Manufacturer</dt><dd>{processor?.manufacturer}</dd></div>
          <div><dt>Status</dt><dd>{processor?.status}</dd></div>
        </dl>
      </div>
    {/each}
  </div>
</section>

<section>
  <h2>Memory</h2>
  <div class="cards">
    {#each data?.memory ?? [undefined] as module}
      <div>
        <h3>{module?.slot}</h3>
        <dl class="card">
          <div><dt>Size</dt><dd>{module?.bytes && `${module.bytes / 2 ** 30} GB`}</dd></div>
          <div><dt>Speed</dt><dd>{module?.mhz && `${module.mhz} MHz`}</dd></div>
          <div><dt>Type</dt><dd>{module?.kind}</dd></div>
          <div><dt>Manufacturer</dt><dd>{module?.manufacturer}</dd></div>
          <div><dt>Part number</dt><dd>{module?.part_number}</dd></div>
          <div><dt>Serial number</dt><dd>{module?.serial_number}</dd></div>
        </dl>
      </div>
    {/each}
  </div>
</section>

{#if !data || data.disks.length > 0}
  <section>
    <h2>Disks</h2>
    <div class="cards">
      {#each data?.disks ?? [undefined] as disk, i}
        <div>
          {#if data && data.disks.length > 1}<h3>Disk {i}</h3>{/if}
          <dl class="card">
            <div><dt>Model</dt><dd>{disk?.model}</dd></div>
            <div><dt>Size</dt><dd>{disk?.bytes && `${Math.round(disk.bytes / 1e9)} GB`}</dd></div>
          </dl>
        </div>
      {/each}
    </div>
  </section>
{/if}
