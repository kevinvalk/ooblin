<script lang="ts">
  import type { Status } from "../../bindings/core";
  import Network from "../Network.svelte";

  let { data }: { data?: Status } = $props();
</script>

<section>
  <h2>Device</h2>
  <dl class="card">
    <div><dt>Name</dt><dd>{data?.info.name}</dd></div>
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
  <h2>Intel AMT</h2>
  <dl class="card">
    <div><dt>Version</dt><dd>{data && data.info.version + (data.info.build ? ` (build ${data.info.build})` : "")}</dd></div>
    <div><dt>Control mode</dt><dd>{data?.info.control_mode}</dd></div>
    <div><dt>Certificate</dt><dd class="font-mono text-xs/5">{data?.info.fingerprint}</dd></div>
  </dl>
</section>

<Network ports={data?.ports} />
