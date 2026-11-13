<!-- The server and its BMC as IPMI tells them, and the BMC's own network port. -->
<script lang="ts">
  import type { IpmiStatus } from "../../bindings/core";
  import Network from "../Network.svelte";

  let { data }: { data?: IpmiStatus } = $props();
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
    <div><dt>System ID</dt><dd class="font-mono">{data?.system_id}</dd></div>
  </dl>
</section>

<section>
  <h2>BMC</h2>
  <dl class="card">
    <div><dt>Manufacturer</dt><dd>{data?.manufacturer}</dd></div>
    <div title="The manufacturer's number for the board or the BMC."><dt>Product ID</dt><dd class="font-mono">{data && `0x${data.product_id.toString(16).toUpperCase().padStart(4, "0")}`}</dd></div>
    <div><dt>Firmware version</dt><dd>{data?.info.version}</dd></div>
    <div><dt>IPMI version</dt><dd>{data?.ipmi_version}</dd></div>
  </dl>
</section>

<Network ports={data?.ports} />
