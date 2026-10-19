<script lang="ts">
  import type { Settings } from "../bindings/core";

  let { data }: { data?: Settings } = $props();
  const yes = (on?: boolean) => on === undefined ? undefined : on ? "Yes" : "No";
</script>

<section>
  <h2>Power policy</h2>
  <dl class="card"><div><dt>Active</dt><dd>{data?.powerPolicy}</dd></div></dl>
</section>

<section>
  <h2>Network</h2>
  <dl class="card"><div><dt>Respond to ping</dt><dd>{yes(data?.respondToPing)}</dd></div></dl>
</section>

<section>
  <h2>System name</h2>
  <dl class="card">
    <div><dt>Host name</dt><dd>{data?.hostName}</dd></div>
    <div><dt>Domain name</dt><dd>{data?.domainName}</dd></div>
    <div><dt>Shared with the OS</dt><dd>{yes(data?.sharedWithOs)}</dd></div>
  </dl>
</section>

<section>
  <h2>User accounts</h2>
  <dl class="card">
    {#each data?.accounts ?? [undefined] as account}
      <div>
        <dt>{account?.name}</dt>
        <dd>{account && (account.admin ? "Administrator" : `${account.realms.join(", ")} (${account.access})`)}</dd>
      </div>
    {/each}
  </dl>
</section>
