<!-- A BMC's own network to change, any flavor's: its host name and DNS server where its flavor sets them, DHCP or its
     addresses, and its VLAN. What may give it another address asks first: we may not reach it there. -->
<script lang="ts">
  import type { BmcNetwork as Network } from "../../bindings/core";
  import Form, { choices, text } from "../Form.svelte";

  let {
    network,
    host,
    save,
    busy,
    ask,
  }: {
    network?: Network | null;
    host: string;
    save: (done: string, change: { Network: Network }) => Promise<boolean>;
    busy: boolean;
    ask: (question: string, answers: string[], safe?: boolean) => Promise<string>;
  } = $props();
  const id = $props.id();

  /** While the form is edited: only a manual configuration takes addresses, only a VLAN that's on its ID. */
  let dhcp = $derived(network?.dhcp ?? true);
  let vlan = $derived(network?.vlan != null);

  async function change(fields: FormData) {
    const changed: Network = {
      hostname: network?.hostname == null ? null : text(fields, "hostname"),
      dhcp: fields.get("configuration") === "dhcp",
      address: text(fields, "address") || network!.address,
      subnet_mask: text(fields, "subnet_mask") || network!.subnet_mask,
      gateway: text(fields, "gateway") || network!.gateway,
      dns: network?.dns == null ? null : text(fields, "dns"),
      vlan: fields.has("vlan") ? Number(fields.get("vlan_id")) : null,
    };
    const moves = changed.dhcp !== network!.dhcp || (!changed.dhcp && changed.address !== network!.address);
    const question = `The BMC may get another address, which ${host} doesn't reach: connect to that one then.`;
    if ((moves || changed.vlan !== network!.vlan) && !(await ask(question, ["Save"]))) return false;
    return save("Network saved.", { Network: changed });
  }
</script>

{#snippet field(name: string, label: string, value: string | undefined, disabled = false)}
  <div>
    <label for="{id}-{name}">{label}</label>
    <input id="{id}-{name}" {name} class="field font-mono" autocapitalize="off" spellcheck="false" defaultValue={value} {disabled} required={!disabled && name !== "dns" && name !== "hostname"} />
  </div>
{/snippet}

{#if network !== null}
  <Form
    title="Network"
    {busy}
    save={(fields) => change(fields)}
    reset={() => ((dhcp = network?.dhcp ?? true), (vlan = network?.vlan != null))}
  >
    {#if network?.hostname !== null}
      {@render field("hostname", "Host name", network?.hostname ?? undefined)}
    {/if}
    <div>
      <label for="{id}-configuration">Configuration</label>
      <select
        id="{id}-configuration"
        name="configuration"
        class="field"
        oninput={(event) => (dhcp = event.currentTarget.value === "dhcp")}
      >
        {@render choices(network && (network.dhcp ? "dhcp" : "manual"), { dhcp: "Automatic (DHCP)", manual: "Manual" })}
      </select>
    </div>
    {@render field("address", "Address", network?.address, dhcp)}
    {@render field("subnet_mask", "Subnet mask", network?.subnet_mask, dhcp)}
    {@render field("gateway", "Gateway", network?.gateway, dhcp)}
    {#if network?.dns !== null}
      {@render field("dns", "DNS server", network?.dns ?? undefined)}
    {/if}
    <div title="Tagged frames on this VLAN only: the switch port has to carry it, or the BMC is gone from the network.">
      <label for="{id}-vlan">VLAN</label>
      <span class="flex items-center gap-4">
        <input id="{id}-vlan" name="vlan" type="checkbox" defaultChecked={network?.vlan != null} oninput={(event) => (vlan = event.currentTarget.checked)} />
        <input
          name="vlan_id"
          type="number"
          class="field w-24"
          min="1"
          max="4094"
          aria-label="VLAN ID"
          defaultValue={network?.vlan ?? undefined}
          disabled={!vlan}
          required={vlan}
        />
      </span>
    </div>
  </Form>
{/if}
