<script module lang="ts">
  /** How an IPv4 address is set, in words, Settings' too. */
  export const CONFIGURATIONS = { Dhcp: "Automatic (DHCP)", Os: "Same as the OS", Manual: "Manual" };
</script>

<script lang="ts">
  import type { Status } from "../../bindings/core";

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

<!-- While loading we show one wired port. -->
{#each data?.ports ?? [undefined] as port (port?.mac)}
  <section>
    <h2>{port?.wireless ? "Wireless network" : "Wired network"}</h2>
    <div class="cards">
      <div class="@3xl:col-span-2">
        <h3>Link</h3>
        <dl class="card">
          <div><dt>MAC address</dt><dd class="font-mono">{port?.mac}</dd></div>
          <div><dt>Status</dt><dd>{port && (port.link_up ? (port.mbps ? `Up, ${port.mbps} Mbit/s` : "Up") : "Down")}</dd></div>
        </dl>
      </div>
      <div>
        <h3>IPv4</h3>
        <dl class="card">
          <div><dt>Configuration</dt><dd>{port && CONFIGURATIONS[port.ipv4.configuration]}</dd></div>
          <div><dt>Address</dt><dd class="font-mono">{port?.ipv4.address}</dd></div>
          <div><dt>Subnet mask</dt><dd class="font-mono">{port?.ipv4.subnet_mask}</dd></div>
          <div><dt>Gateway</dt><dd class="font-mono">{port?.ipv4.gateway}</dd></div>
          <div><dt>Preferred DNS</dt><dd class="font-mono">{port?.ipv4.primary_dns}</dd></div>
          <div><dt>Alternate DNS</dt><dd class="font-mono">{port?.ipv4.secondary_dns}</dd></div>
        </dl>
      </div>
      <div>
        <h3>IPv6</h3>
        <dl class="card">
          {#if port && !port.ipv6}
            <div><dt>Status</dt><dd>Disabled</dd></div>
          {:else}
            <div><dt>Addresses</dt><dd class="font-mono">{port?.ipv6?.addresses.join("\n")}</dd></div>
            <div><dt>Gateway</dt><dd class="font-mono">{port?.ipv6?.gateway}</dd></div>
            <div><dt>Preferred DNS</dt><dd class="font-mono">{port?.ipv6?.primary_dns}</dd></div>
            <div><dt>Alternate DNS</dt><dd class="font-mono">{port?.ipv6?.secondary_dns}</dd></div>
          {/if}
        </dl>
      </div>
    </div>
  </section>
{/each}
