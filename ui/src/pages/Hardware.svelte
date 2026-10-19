<script lang="ts">
  import type { Hardware } from "../bindings/core";

  /** While loading, every list shows one of its thing. Several are told apart by number, from 0 like the OS does. */
  let { data }: { data?: Hardware } = $props();
</script>

<section>
  <h2>System</h2>
  <dl class="card">
    <div><dt>Model</dt><dd>{data?.system.model}</dd></div>
    <div><dt>Manufacturer</dt><dd>{data?.system.manufacturer}</dd></div>
    <div><dt>Version</dt><dd>{data?.system.version}</dd></div>
    <div><dt>Serial number</dt><dd>{data?.system.serial_number}</dd></div>
    <div><dt>System ID</dt><dd class="font-mono">{data?.system.id}</dd></div>
    {#each data?.processors ?? [undefined] as processor, i}
      <div>
        <dt>{data && data.processors.length > 1 ? `CPU ${i}` : "CPU"}</dt>
        <dd>{processor && `${processor.model} @ ${[processor.mhz, processor.max_mhz].filter(Boolean).join(" / ")} MHz`}</dd>
      </div>
    {/each}
  </dl>
</section>

<section>
  <h2>Motherboard</h2>
  <dl class="card">
    <div><dt>Product</dt><dd>{data?.motherboard.product}</dd></div>
    <div><dt>Manufacturer</dt><dd>{data?.motherboard.manufacturer}</dd></div>
    <div><dt>Version</dt><dd>{data?.motherboard.version}</dd></div>
    <div><dt>Serial number</dt><dd>{data?.motherboard.serial_number}</dd></div>
    <div><dt>Asset tag</dt><dd>{data?.motherboard.asset_tag}</dd></div>
    <div><dt>Replaceable</dt><dd>{data && (data.motherboard.replaceable ? "Yes" : "No")}</dd></div>
  </dl>
</section>

<section>
  <h2>BIOS</h2>
  <dl class="card">
    <div><dt>Vendor</dt><dd>{data?.bios.vendor}</dd></div>
    <div><dt>Version</dt><dd>{data?.bios.version}</dd></div>
    <div><dt>Release date</dt><dd>{data?.bios.released}</dd></div>
    <div><dt>Supports</dt><dd>{data?.bios.features.join(", ")}</dd></div>
  </dl>
</section>

<section>
  <h2>Processors</h2>
  <div class="cards">
    {#each data?.processors ?? [undefined] as processor, i}
      <div>
        {#if data && data.processors.length > 1}<h3>Processor {i}</h3>{/if}
        <dl class="card">
          <div><dt>Model</dt><dd>{processor?.model}</dd></div>
          <div><dt>Manufacturer</dt><dd>{processor?.manufacturer}</dd></div>
          <div><dt>Family</dt><dd>{processor?.family}</dd></div>
          <div><dt>Socket</dt><dd>{processor?.socket}</dd></div>
          <div><dt>Speed</dt><dd>{processor?.mhz && `${processor.mhz} MHz`}</dd></div>
          <div><dt>Maximum speed</dt><dd>{processor?.max_mhz && `${processor.max_mhz} MHz`}</dd></div>
          <div><dt>Status</dt><dd>{processor?.status}</dd></div>
          <div><dt>Upgrade method</dt><dd>{processor?.upgrade_method}</dd></div>
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
          <div><dt>Form factor</dt><dd>{module?.form_factor}</dd></div>
          <div><dt>Manufacturer</dt><dd>{module?.manufacturer}</dd></div>
          <div><dt>Part number</dt><dd>{module?.part_number}</dd></div>
          <div><dt>Serial number</dt><dd>{module?.serial_number}</dd></div>
          <div><dt>Asset tag</dt><dd>{module?.asset_tag}</dd></div>
        </dl>
      </div>
    {/each}
  </div>
</section>

<section>
  <h2>Disks</h2>
  <div class="cards">
    {#each data?.disks ?? [undefined] as disk, i}
      <div>
        {#if data && data.disks.length > 1}<h3>Disk {i}</h3>{/if}
        <dl class="card">
          <div><dt>Model</dt><dd>{disk?.model}</dd></div>
          <div><dt>Serial number</dt><dd>{disk?.serial_number}</dd></div>
          <div><dt>Size</dt><dd>{disk?.bytes && `${Math.round(disk.bytes / 1e9)} GB`}</dd></div>
        </dl>
      </div>
    {/each}
  </div>
</section>

<!-- The device lists no battery when it has none. -->
<section>
  <h2>Battery</h2>
  <div class="cards">
    {#each data?.batteries ?? [] as battery, i}
      <div>
        {#if data && data.batteries.length > 1}<h3>Battery {i}</h3>{/if}
        <dl class="card">
          <div><dt>Name</dt><dd>{battery.name}</dd></div>
          <div><dt>Charge</dt><dd>{battery.charge_percent && `${battery.charge_percent} %`}</dd></div>
          <div><dt>Design capacity</dt><dd>{battery.design_capacity_mwh && `${battery.design_capacity_mwh / 1000} Wh`}</dd></div>
        </dl>
      </div>
    {:else}
      <dl class="card"><div><dt>Status</dt><dd>{data && "Not installed"}</dd></div></dl>
    {/each}
  </div>
</section>
