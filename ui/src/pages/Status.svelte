<script lang="ts">
  import type { Status } from "../bindings/core";

  let { data }: { data?: Status } = $props();
  const enabled = (on?: boolean) => on === undefined ? undefined : on ? "Enabled" : "Disabled";
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
    <div><dt>System ID</dt><dd>{data?.system_id}</dd></div>
    <div><dt>Clock</dt><dd>{data?.clock && new Date(data.clock * 1000).toLocaleString()}</dd></div>
  </dl>
</section>

<section>
  <h2>Intel AMT</h2>
  <dl class="card">
    <div><dt>Version</dt><dd>{data && data.info.version + (data.info.build ? ` (build ${data.info.build})` : "")}</dd></div>
    <div><dt>Control mode</dt><dd>{data?.info.control_mode}</dd></div>
    <div><dt>Certificate</dt><dd>{data?.info.fingerprint}</dd></div>
  </dl>
</section>

<section>
  <h2>Remote access</h2>
  <dl class="card">
    <div><dt>Redirection</dt><dd>{enabled(data?.redirection)}</dd></div>
    <div><dt>Serial over LAN</dt><dd>{enabled(data?.serial_over_lan)}</dd></div>
    <div><dt>IDE redirection</dt><dd>{enabled(data?.ide_redirection)}</dd></div>
    <div><dt>KVM</dt><dd>{enabled(data?.kvm)}</dd></div>
    <div><dt>User consent</dt><dd>{data?.user_consent}</dd></div>
  </dl>
</section>

<!-- While loading we show one wired port. -->
{#each data?.ports ?? [undefined] as port}
  <section>
    <h2>{port?.wireless ? "Wireless network" : "Wired network"}</h2>
    <div class="grid gap-x-4 gap-y-6 @3xl:grid-cols-2">
      <div class="@3xl:col-span-2">
        <h3>Link</h3>
        <dl class="card">
          <div><dt>MAC address</dt><dd>{port?.mac}</dd></div>
          <div><dt>Status</dt><dd>{port && (port.link_up ? (port.mbps ? `Up, ${port.mbps} Mbit/s` : "Up") : "Down")}</dd></div>
        </dl>
      </div>
      <div>
        <h3>IPv4</h3>
        <dl class="card">
          <div><dt>Configuration</dt><dd>{port && (port.ipv4.dhcp ? "Automatic (DHCP)" : "Manual")}</dd></div>
          <div><dt>Address</dt><dd>{port?.ipv4.address}</dd></div>
          <div><dt>Subnet mask</dt><dd>{port?.ipv4.subnet_mask}</dd></div>
          <div><dt>Gateway</dt><dd>{port?.ipv4.gateway}</dd></div>
          <div><dt>Preferred DNS</dt><dd>{port?.ipv4.primary_dns}</dd></div>
          <div><dt>Alternate DNS</dt><dd>{port?.ipv4.secondary_dns}</dd></div>
        </dl>
      </div>
      <div>
        <h3>IPv6</h3>
        <dl class="card">
          {#if port && !port.ipv6}
            <div><dt>Status</dt><dd>Disabled</dd></div>
          {:else}
            <div><dt>Addresses</dt><dd>{port?.ipv6?.addresses.join("\n")}</dd></div>
            <div><dt>Gateway</dt><dd>{port?.ipv6?.gateway}</dd></div>
            <div><dt>Preferred DNS</dt><dd>{port?.ipv6?.primary_dns}</dd></div>
            <div><dt>Alternate DNS</dt><dd>{port?.ipv6?.secondary_dns}</dd></div>
          {/if}
        </dl>
      </div>
    </div>
  </section>
{/each}
