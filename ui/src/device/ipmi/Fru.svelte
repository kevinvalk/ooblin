<!-- A BMC's FRU (its field-replaceable unit information, which the OS reads as the chassis', board's and product's) to
     change, an area at a time, any BMC flavor's: what the FRU has no area for it can't hold. The BMC rewrites an area
     where it has room (an area may grow into what no other holds) and checks what it wrote. -->
<script lang="ts">
  import type { Fru } from "../../bindings/core";
  import Form, { choices, text } from "../Form.svelte";

  let {
    fru,
    save,
    busy,
  }: { fru?: Fru | null; save: (done: string, change: { Fru: Fru }) => Promise<boolean>; busy: boolean } = $props();
  const id = $props.id();

  /** Each area's text fields by name, in the order the FRU holds them. */
  const AREAS = {
    chassis: { title: "Chassis", fields: { part_number: "Part number", serial_number: "Serial number" } },
    board: {
      title: "Board",
      fields: { manufacturer: "Manufacturer", product: "Product name", serial_number: "Serial number", part_number: "Part number" },
    },
    product: {
      title: "Product",
      fields: {
        manufacturer: "Manufacturer",
        name: "Name",
        part_number: "Part number",
        version: "Version",
        serial_number: "Serial number",
        asset_tag: "Asset tag",
      },
    },
  } as const;

  /** SMBIOS's chassis types, by number. */
  const TYPES = [
    "Unspecified", "Other", "Unknown", "Desktop", "Low profile desktop", "Pizza box", "Mini tower", "Tower", "Portable",
    "Laptop", "Notebook", "Hand held", "Docking station", "All in one", "Sub notebook", "Space-saving", "Lunch box",
    "Main server chassis", "Expansion chassis", "Subchassis", "Bus expansion chassis", "Peripheral chassis",
    "RAID chassis", "Rack mount chassis", "Sealed-case PC", "Multi-system chassis", "CompactPCI", "AdvancedTCA", "Blade",
    "Blade enclosure", "Tablet", "Convertible", "Detachable", "IoT gateway", "Embedded PC", "Mini PC", "Stick PC",
  ];
</script>

<div class="grid gap-6">
  {#each Object.entries(AREAS) as [area, { title, fields }] (area)}
    {@const values = fru?.[area as keyof typeof AREAS] as Record<string, string | number> | null | undefined}
    {#if fru === undefined || values}
      <Form
        {title}
        {busy}
        save={(form) => {
          const changed = Object.fromEntries(Object.keys(fields).map((field) => [field, text(form, field)]));
          const kind = area === "chassis" ? { kind: Number(form.get("kind")) } : {};
          return save(`${title} saved.`, { Fru: { ...fru!, [area]: { ...values, ...changed, ...kind } } });
        }}
      >
        {#if area === "chassis"}
          <div>
            <label for="{id}-chassis-kind">Type</label>
            <select id="{id}-chassis-kind" name="kind" class="field">
              {@render choices(values ? String(values.kind) : undefined, Object.fromEntries(TYPES.map((type, kind) => [kind, type])))}
            </select>
          </div>
        {/if}
        {#each Object.entries(fields) as [field, label] (field)}
          <div>
            <label for="{id}-{area}-{field}">{label}</label>
            <input
              id="{id}-{area}-{field}"
              name={field}
              class="field"
              autocapitalize="off"
              spellcheck="false"
              maxlength="63"
              defaultValue={values?.[field] as string | undefined}
            />
          </div>
        {/each}
      </Form>
    {/if}
  {/each}
</div>
