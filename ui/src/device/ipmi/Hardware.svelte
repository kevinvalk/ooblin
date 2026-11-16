<script lang="ts">
  import type { Fru } from "../../bindings/core";

  let { data }: { data?: Fru } = $props();
</script>

{#if data && !data.chassis && !data.board && !data.product}
  <p class="py-16 text-center text-muted">The BMC's FRU is empty.</p>
{/if}

{#if !data || data.product}
  <section>
    <h2>Product</h2>
    <dl class="card">
      <div><dt>Manufacturer</dt><dd>{data?.product?.manufacturer}</dd></div>
      <div><dt>Name</dt><dd>{data?.product?.name}</dd></div>
      <div><dt>Part number</dt><dd>{data?.product?.part_number}</dd></div>
      <div><dt>Version</dt><dd>{data?.product?.version}</dd></div>
      <div><dt>Serial number</dt><dd>{data?.product?.serial_number}</dd></div>
      <div><dt>Asset tag</dt><dd>{data?.product?.asset_tag}</dd></div>
    </dl>
  </section>
{/if}

{#if !data || data.board}
  <section>
    <h2>Board</h2>
    <dl class="card">
      <div><dt>Manufacturer</dt><dd>{data?.board?.manufacturer}</dd></div>
      <div><dt>Product name</dt><dd>{data?.board?.product}</dd></div>
      <div><dt>Serial number</dt><dd>{data?.board?.serial_number}</dd></div>
      <div><dt>Part number</dt><dd>{data?.board?.part_number}</dd></div>
      <div><dt>Manufactured</dt><dd>{data?.board?.manufactured_at && new Date(data.board.manufactured_at * 1000).toLocaleDateString()}</dd></div>
    </dl>
  </section>
{/if}

{#if !data || data.chassis}
  <section>
    <h2>Chassis</h2>
    <dl class="card">
      <div><dt>Part number</dt><dd>{data?.chassis?.part_number}</dd></div>
      <div><dt>Serial number</dt><dd>{data?.chassis?.serial_number}</dd></div>
    </dl>
  </section>
{/if}
