<script lang="ts">
  import type { Settings } from "../bindings/core";

  let { data }: { data?: Settings } = $props();
  const yes = (on?: boolean) => on === undefined ? undefined : on ? "Yes" : "No";
</script>

<section>
  <h2>Power policy</h2>
  <dl class="card"><div><dt>Active</dt><dd>{data?.power_policy}</dd></div></dl>
</section>

<section>
  <h2>Network</h2>
  <dl class="card"><div><dt>Respond to ping</dt><dd>{yes(data?.respond_to_ping)}</dd></div></dl>
</section>

<section>
  <h2>System name</h2>
  <dl class="card">
    <div><dt>Host name</dt><dd>{data?.host_name}</dd></div>
    <div><dt>Domain name</dt><dd>{data?.domain_name}</dd></div>
    <div><dt>Shared with the OS</dt><dd>{yes(data?.shared_with_os)}</dd></div>
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
