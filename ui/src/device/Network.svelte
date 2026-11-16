<script module lang="ts">
  /** How an IPv4 address is set, in words. The settings of AMT say it the same way. */
  export const CONFIGURATIONS = { Dhcp: "Automatic (DHCP)", Os: "Same as the OS", Manual: "Manual" };
</script>

<script lang="ts">
  import type { Port } from "../bindings/core";

  let { ports }: { ports?: Port[] } = $props();
</script>

<!-- While loading we show one wired port. -->
{#each ports ?? [undefined] as port (port?.mac)}
  <section>
    <h2>{port?.is_wireless ? "Wireless network" : "Wired network"}</h2>
    <div class="cards">
      <div class="@3xl:col-span-2">
        <h3>Link</h3>
        <dl class="card">
          <div><dt>MAC address</dt><dd class="font-mono">{port?.mac}</dd></div>
          <div><dt>Status</dt><dd>{port && (port.is_link_up ? (port.mbps ? `Up, ${port.mbps} Mbit/s` : "Up") : "Down")}</dd></div>
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
