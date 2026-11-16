<script lang="ts">
  import type { Event } from "../bindings/core";

  let { data }: { data?: Event[] } = $props();
</script>

{#if data?.length === 0}
  <p class="py-16 text-center text-muted">Nothing logged.</p>
{:else}
  <table class="w-full border-separate border-spacing-0">
    <thead><tr><th>Time</th><th>Source</th><th>Description</th></tr></thead>
    <tbody>
      {#each data ?? Array(12) as event}
        <tr data-severity={event?.severity} title={event?.severity}>
          <td>{event && new Date(event.occurred_at * 1000).toLocaleString()}</td>
          <td>{event?.source}</td>
          <td>{event?.description}</td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}
