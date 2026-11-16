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
    ask: (question: string, answers: string[], isSafe?: boolean) => Promise<string>;
  } = $props();
  const id = $props.id();

  /** What the form has while it is `edited`, as only a manual configuration takes addresses and only a VLAN that is on
   * takes its ID. An edit outlasts the settings, which land again after every save, one that failed too. */
  let edited = $state<{ dhcp?: boolean; vlan?: boolean }>({});
  let isDhcp = $derived(edited.dhcp ?? network?.is_dhcp ?? true);
  let hasVlan = $derived(edited.vlan ?? network?.vlan != null);

  async function change(fields: FormData) {
    // The host name and DNS server are null where the flavor of the BMC does not set them.
    const changed: Network = {
      hostname: network?.hostname == null ? null : text(fields, "hostname"),
      is_dhcp: fields.get("configuration") === "dhcp",
      address: text(fields, "address") || network!.address,
      subnet_mask: text(fields, "subnet_mask") || network!.subnet_mask,
      gateway: text(fields, "gateway") || network!.gateway,
      dns: network?.dns == null ? null : text(fields, "dns"),
      vlan: fields.has("vlan") ? Number(fields.get("vlan_id")) : null,
    };

    // What may give the BMC another address asks first, as we may not reach it there.
    const moves = changed.is_dhcp !== network!.is_dhcp || (!changed.is_dhcp && changed.address !== network!.address);
    const question = `The BMC may get another address, which ${host} doesn't reach: connect to that one then.`;
    if ((moves || changed.vlan !== network!.vlan) && (await ask(question, ["Save"])) === "") return false;

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
    save={change}
    reset={() => (edited = {})}
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
        oninput={(event) => (edited.dhcp = event.currentTarget.value === "dhcp")}
      >
        {@render choices(network && (network.is_dhcp ? "dhcp" : "manual"), { dhcp: "Automatic (DHCP)", manual: "Manual" })}
      </select>
    </div>
    {@render field("address", "Address", network?.address, isDhcp)}
    {@render field("subnet_mask", "Subnet mask", network?.subnet_mask, isDhcp)}
    {@render field("gateway", "Gateway", network?.gateway, isDhcp)}
    {#if network?.dns !== null}
      {@render field("dns", "DNS server", network?.dns ?? undefined)}
    {/if}
    <div title="Tagged frames on this VLAN only: the switch port has to carry it, or the BMC is gone from the network.">
      <label for="{id}-vlan">VLAN</label>
      <span class="flex items-center gap-4">
        <input id="{id}-vlan" name="vlan" type="checkbox" defaultChecked={network?.vlan != null} oninput={(event) => (edited.vlan = event.currentTarget.checked)} />
        <input
          name="vlan_id"
          type="number"
          class="field w-24"
          min="1"
          max="4094"
          aria-label="VLAN ID"
          defaultValue={network?.vlan ?? undefined}
          disabled={!hasVlan}
          required={hasVlan}
        />
      </span>
    </div>
  </Form>
{/if}
