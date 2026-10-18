<script lang="ts">
  import type { Event } from "../bindings/core";

  let { data }: { data?: Event[] } = $props();
</script>

{#if data?.length === 0}
  <p class="py-16 text-center text-muted">Nothing logged.</p>
{:else}
  <table class="w-full border-separate border-spacing-0">
    <thead><tr><th>Time (UTC)</th><th>Source</th><th>Description</th></tr></thead>
    <tbody>
      {#each data ?? Array(12) as event}
        <tr>
          <td>{event?.time}</td>
          <td>{event?.source}</td>
          <!-- Warnings and critical events stand out by colour. -->
          <td
            class="data-[severity=Critical]:text-danger data-[severity=Warning]:text-asleep"
            data-severity={event?.severity}
            title={event?.severity}>{event?.description}</td
          >
        </tr>
      {/each}
    </tbody>
  </table>
{/if}
