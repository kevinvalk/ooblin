<!-- What can be set over IPMI: the BMC's network, the server's settings, its FRU, and the BMC's users to read. -->
<script lang="ts">
  import type { IpmiChange, IpmiSettings } from "../../bindings/core";
  import type { Connection } from "../Device.svelte";
  import Fru from "./Fru.svelte";
  import NetworkSettings from "./NetworkSettings.svelte";
  import Server from "./Server.svelte";

  let {
    data,
    connection,
    save,
    busy,
    ask,
  }: {
    data?: IpmiSettings;
    connection: Connection;
    save: (done: string, change: IpmiChange) => Promise<boolean>;
    busy: boolean;
    ask: (question: string, answers: string[], safe?: boolean) => Promise<string>;
  } = $props();
</script>

<section>
  <h2>BMC</h2>
  <NetworkSettings network={data && data.network} host={connection.host} {save} {busy} {ask} />
</section>

<section>
  <h2>Server</h2>
  <Server loaded={!!data} restore={data?.power_restore} sol={data?.sol} {save} {busy} />
</section>

<section>
  <h2>FRU</h2>
  <Fru fru={data && data.fru} {save} {busy} />
</section>

<section>
  <h2>Users</h2>
  <dl class="card">
    {#each data?.users ?? [undefined] as user (user?.id)}
      <div title={user && `User ${user.id}`}>
        <dt>{user?.name}</dt>
        <dd>{user && user.privilege + (user.enabled === false ? ", disabled" : "")}</dd>
      </div>
    {/each}
  </dl>
</section>
