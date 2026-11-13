<!-- A BMC's sensors, any flavor's, by what they measure. Warnings and critical readings stand out by color, as in the
     event log; between its critical thresholds a reading is a bar too. A sensor that only has states says them. -->
<script lang="ts">
  import type { Sensor, SensorKind } from "../../bindings/core";

  let { data }: { data?: Sensor[] } = $props();

  const KINDS: Record<SensorKind, string> = {
    Temperature: "Temperatures",
    Fan: "Fans",
    Voltage: "Voltages",
    Current: "Currents",
    Power: "Power",
    Other: "Other",
  };
</script>

{#if data?.length === 0}
  <p class="py-16 text-center text-muted">No sensors.</p>
{/if}
<!-- While loading every group shows one sensor. -->
{#each Object.entries(KINDS) as [kind, title] (kind)}
  {@const sensors = data ? data.filter((sensor) => sensor.kind === kind) : [undefined]}
  {#if sensors.length}
    <section>
      <h2>{title}</h2>
      <dl class="card">
        {#each sensors as sensor}
          <div
            class="data-[severity=Critical]:text-danger data-[severity=Warning]:text-asleep"
            data-severity={sensor?.severity}
            title={sensor?.severity}
          >
            <dt>{sensor?.name}</dt>
            <dd class="flex items-center gap-4">{#if sensor}
                {@const { reading, unit, state, low, high } = sensor}
                <span class={state ? "" : "w-24 flex-none"}>
                  {reading !== null ? `${reading} ${unit}` : state || "Absent"}
                </span>
                {#if reading !== null && low !== null && high !== null && low < high}
                  <meter min={low} max={high} value={reading} title="Critical under {low} and over {high} {unit}"></meter>
                {/if}
              {/if}</dd>
          </div>
        {/each}
      </dl>
    </section>
  {/if}
{/each}
