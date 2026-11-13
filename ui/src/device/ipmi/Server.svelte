<!-- The server's settings a BMC keeps over IPMI, any flavor's: what it does when power comes back, and how fast its
     serial console runs. Each saves as it changes; one the BMC doesn't say can't change. -->
<script lang="ts">
  import type { PowerRestore, SolSettings } from "../../bindings/core";
  import Form, { choices } from "../Form.svelte";

  let {
    loaded,
    restore,
    sol,
    save,
    busy,
  }: {
    loaded: boolean;
    restore?: PowerRestore | null;
    sol?: SolSettings | null;
    save: (done: string, change: { PowerRestore: PowerRestore } | { SolBitRate: number }) => Promise<boolean>;
    busy: boolean;
  } = $props();
  const id = $props.id();

  const RESTORE: Record<PowerRestore, string> = { StayOff: "Stays off", Previous: "As it was", PowerOn: "Powers on" };
  const BIT_RATES = [9600, 19200, 38400, 57600, 115200];
</script>

{#snippet select(name: string, label: string, current: string | undefined, options: Record<string, string>, help: string)}
  <div title={loaded && !Object.keys(options).length ? "This BMC doesn't say." : help}>
    <label for="{id}-{name}">{label}</label>
    <select id="{id}-{name}" {name} class="field" disabled={loaded && !Object.keys(options).length}>
      {@render choices(current, options)}
    </select>
  </div>
{/snippet}

<Form
  {busy}
  save={(fields, done, name) =>
    save(
      done,
      name === "restore"
        ? { PowerRestore: fields.get("restore") as PowerRestore }
        : { SolBitRate: Number(fields.get("bit_rate")) },
    )}
>
  {@render select(
    "restore",
    "When power comes back",
    restore ?? undefined,
    restore ? RESTORE : {},
    "What the server does once its power was cut and comes back.",
  )}
  {@render select(
    "bit_rate",
    "Serial console speed",
    String(sol?.bit_rate ?? ""),
    sol ? { ...(sol.bit_rate ? {} : { "": "As the BMC has it" }), ...Object.fromEntries(BIT_RATES.map((rate) => [rate, `${rate} bit/s`])) } : {},
    "How fast the server's serial port runs, as its BIOS and OS have to: 115200 for most.",
  )}
</Form>
