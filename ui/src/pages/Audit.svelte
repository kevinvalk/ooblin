<script lang="ts">
  import type { AuditRecord } from "../bindings/core";

  let { data }: { data?: AuditRecord[] } = $props();
</script>

{#if data?.length === 0}
  <p class="py-16 text-center text-muted">Nothing logged.</p>
{:else}
  <table class="w-full border-separate border-spacing-0">
    <thead><tr><th>Time (UTC)</th><th>Application</th><th>Event</th><th>By</th><th>Details</th></tr></thead>
    <tbody>
      {#each data ?? Array(12) as record}
        <tr>
          <td>{record?.time}</td>
          <td>{record?.application}</td>
          <td>{record?.event}</td>
          <td>{record?.by}</td>
          <td>{record?.details}</td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}
