<script lang="ts">
  import type { Hardware } from "../bindings/core";

  /** While loading, every list shows one of its thing. */
  let { data }: { data?: Hardware } = $props();
</script>

<section>
  <h2>System</h2>
  <dl class="card">
    <div><dt>Model</dt><dd>{data?.system.model}</dd></div>
    <div><dt>Manufacturer</dt><dd>{data?.system.manufacturer}</dd></div>
    <div><dt>Version</dt><dd>{data?.system.version}</dd></div>
    <div><dt>Serial number</dt><dd>{data?.system.serialNumber}</dd></div>
    <div><dt>System ID</dt><dd>{data?.system.id}</dd></div>
    {#each data?.processors ?? [undefined] as processor, i}
      <div><dt>CPU {i + 1}</dt><dd>{processor?.summary}</dd></div>
    {/each}
  </dl>
</section>

<section>
  <h2>Motherboard</h2>
  <dl class="card">
    <div><dt>Product</dt><dd>{data?.motherboard.product}</dd></div>
    <div><dt>Manufacturer</dt><dd>{data?.motherboard.manufacturer}</dd></div>
    <div><dt>Version</dt><dd>{data?.motherboard.version}</dd></div>
    <div><dt>Serial number</dt><dd>{data?.motherboard.serialNumber}</dd></div>
    <div><dt>Asset tag</dt><dd>{data?.motherboard.assetTag}</dd></div>
    <div><dt>Replaceable</dt><dd>{data?.motherboard.replaceable}</dd></div>
  </dl>
</section>

<section>
  <h2>BIOS</h2>
  <dl class="card">
    <div><dt>Vendor</dt><dd>{data?.bios.vendor}</dd></div>
    <div><dt>Version</dt><dd>{data?.bios.version}</dd></div>
    <div><dt>Release date</dt><dd>{data?.bios.released}</dd></div>
    <div><dt>Supports</dt><dd>{data?.bios.features}</dd></div>
  </dl>
</section>

<section>
  <h2>Processors</h2>
  <div class="grid gap-x-4 gap-y-6 @3xl:grid-cols-2">
    {#each data?.processors ?? [undefined] as processor, i}
      <div>
        <h3>Processor {i + 1}</h3>
        <dl class="card">
          <div><dt>Model</dt><dd>{processor?.model}</dd></div>
          <div><dt>Manufacturer</dt><dd>{processor?.manufacturer}</dd></div>
          <div><dt>Family</dt><dd>{processor?.family}</dd></div>
          <div><dt>Socket</dt><dd>{processor?.socket}</dd></div>
          <div><dt>Speed</dt><dd>{processor?.speed}</dd></div>
          <div><dt>Maximum speed</dt><dd>{processor?.maxSpeed}</dd></div>
          <div><dt>Status</dt><dd>{processor?.status}</dd></div>
          <div><dt>Upgrade method</dt><dd>{processor?.upgradeMethod}</dd></div>
        </dl>
      </div>
    {/each}
  </div>
</section>

<section>
  <h2>Memory</h2>
  <div class="grid gap-x-4 gap-y-6 @3xl:grid-cols-2">
    {#each data?.memory ?? [undefined] as module}
      <div>
        <h3>{module?.slot}</h3>
        <dl class="card">
          <div><dt>Size</dt><dd>{module?.size}</dd></div>
          <div><dt>Speed</dt><dd>{module?.speed}</dd></div>
          <div><dt>Type</dt><dd>{module?.kind}</dd></div>
          <div><dt>Form factor</dt><dd>{module?.formFactor}</dd></div>
          <div><dt>Manufacturer</dt><dd>{module?.manufacturer}</dd></div>
          <div><dt>Part number</dt><dd>{module?.partNumber}</dd></div>
          <div><dt>Serial number</dt><dd>{module?.serialNumber}</dd></div>
          <div><dt>Asset tag</dt><dd>{module?.assetTag}</dd></div>
        </dl>
      </div>
    {/each}
  </div>
</section>

<section>
  <h2>Disks</h2>
  <div class="grid gap-x-4 gap-y-6 @3xl:grid-cols-2">
    {#each data?.disks ?? [undefined] as disk, i}
      <div>
        <h3>Disk {i + 1}</h3>
        <dl class="card">
          <div><dt>Model</dt><dd>{disk?.model}</dd></div>
          <div><dt>Serial number</dt><dd>{disk?.serialNumber}</dd></div>
          <div><dt>Size</dt><dd>{disk?.size}</dd></div>
        </dl>
      </div>
    {/each}
  </div>
</section>

<!-- The device lists no battery when it has none. -->
<section>
  <h2>Battery</h2>
  <div class="grid gap-x-4 gap-y-6 @3xl:grid-cols-2">
    {#each data?.batteries ?? [] as battery, i}
      <div>
        <h3>Battery {i + 1}</h3>
        <dl class="card">
          <div><dt>Name</dt><dd>{battery.name}</dd></div>
          <div><dt>Charge</dt><dd>{battery.charge}</dd></div>
          <div><dt>Design capacity</dt><dd>{battery.designCapacity}</dd></div>
        </dl>
      </div>
    {:else}
      <dl class="card"><div><dt>Status</dt><dd>{data && "Not installed"}</dd></div></dl>
    {/each}
  </div>
</section>
