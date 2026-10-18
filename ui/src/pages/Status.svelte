<script lang="ts">
  import type { Status } from "../bindings/core";

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
    <div><dt>System ID</dt><dd>{data?.systemId}</dd></div>
    <div><dt>Clock (UTC)</dt><dd>{data?.clock}</dd></div>
  </dl>
</section>

<section>
  <h2>Intel AMT</h2>
  <dl class="card">
    <div><dt>Version</dt><dd>{data?.info.version}</dd></div>
    <div><dt>Control mode</dt><dd>{data?.info.controlMode}</dd></div>
    <div><dt>Certificate</dt><dd>{data?.info.fingerprint}</dd></div>
  </dl>
</section>

<section>
  <h2>Remote access</h2>
  <dl class="card">
    <div><dt>Redirection</dt><dd>{data?.remoteAccess.redirection}</dd></div>
    <div><dt>Serial over LAN</dt><dd>{data?.remoteAccess.serialOverLan}</dd></div>
    <div><dt>IDE redirection</dt><dd>{data?.remoteAccess.ideRedirection}</dd></div>
    <div><dt>KVM</dt><dd>{data?.remoteAccess.kvm}</dd></div>
    <div><dt>User consent</dt><dd>{data?.remoteAccess.userConsent}</dd></div>
  </dl>
</section>

<!-- While loading we show one wired port. -->
{#each data?.ports ?? [undefined] as port}
  <section>
    <h2>{port?.name ?? "Wired network"}</h2>
    <div class="grid gap-x-4 gap-y-6 @3xl:grid-cols-2">
      <div class="@3xl:col-span-2">
        <h3>Link</h3>
        <dl class="card">
          <div><dt>MAC address</dt><dd>{port?.mac}</dd></div>
          <div><dt>Status</dt><dd>{port?.link}</dd></div>
        </dl>
      </div>
      <div>
        <h3>IPv4</h3>
        <dl class="card">
          <div><dt>Configuration</dt><dd>{port?.ipv4.configuration}</dd></div>
          <div><dt>Address</dt><dd>{port?.ipv4.address}</dd></div>
          <div><dt>Subnet mask</dt><dd>{port?.ipv4.subnetMask}</dd></div>
          <div><dt>Gateway</dt><dd>{port?.ipv4.gateway}</dd></div>
          <div><dt>Preferred DNS</dt><dd>{port?.ipv4.primaryDns}</dd></div>
          <div><dt>Alternate DNS</dt><dd>{port?.ipv4.secondaryDns}</dd></div>
        </dl>
      </div>
      <div>
        <h3>IPv6</h3>
        <dl class="card">
          {#if port && !port.ipv6}
            <div><dt>Status</dt><dd>Disabled</dd></div>
          {:else}
            <div><dt>Addresses</dt><dd>{port?.ipv6?.addresses}</dd></div>
            <div><dt>Gateway</dt><dd>{port?.ipv6?.gateway}</dd></div>
            <div><dt>Preferred DNS</dt><dd>{port?.ipv6?.primaryDns}</dd></div>
            <div><dt>Alternate DNS</dt><dd>{port?.ipv6?.secondaryDns}</dd></div>
          {/if}
        </dl>
      </div>
    </div>
  </section>
{/each}
