<!-- A card of settings, AMT's or a BMC's. With a `title` its fields go together and save together, from the title's
     Save and Cancel (or Enter and Escape), there all along so it's clear what saves this way and enabled once something
     changed. Without one each switch or choice saves as it changes. `save` gets the form's fields and what changed in
     words ("IPv4 saved.", "KVM turned off."), the changed control's name too; one that says it didn't go through keeps
     what was typed, to try again or cancel. Either way the form then shows what the device took: the settings are the
     fields' defaults. -->
<script module lang="ts">
  /** A select's options, the device's `current` one selected: unlike the select's own default, a selected option counts
   * when it comes in after the select, the settings being read. A `current` none of them is shows as it is, so saving
   * what's around it doesn't change it. */
  export { choices };

  /** A text field's value, trimmed. */
  export const text = (fields: FormData, name: string) => ((fields.get(name) as string | null) ?? "").trim();

  /** What a control just changed to, in words: "KVM turned off.", "User consent set to For KVM.". */
  function changedTo(control: HTMLInputElement | HTMLSelectElement) {
    const label = control.labels?.[0].textContent;
    if (control instanceof HTMLSelectElement) return `${label} set to ${control.selectedOptions[0].text}.`;
    return `${label} turned ${control.checked ? "on" : "off"}.`;
  }
</script>

<script lang="ts">
  import { Check, LoaderCircle, X } from "@lucide/svelte";
  import { tick, type Snippet } from "svelte";

  let {
    title,
    busy,
    save,
    reset = () => {},
    children,
  }: {
    title?: string;
    /** While the settings are read or anything saves. */
    busy: boolean;
    save: (fields: FormData, done: string, name: string) => Promise<unknown>;
    reset?: () => void;
    children: Snippet;
  } = $props();
  const id = $props.id();
  let changed = $state(false);

  /** Saves, then shows what the device took, unless what was typed is kept. */
  async function apply(form: HTMLFormElement, done: string, name = "") {
    if (!(await save(new FormData(form), done, name)) && title) return;
    await tick();
    form.reset();
  }
</script>

{#snippet choices(current: string | undefined, options: Record<string, string>)}
  {#if current !== undefined && !(current in options)}<option value={current} selected>{current}</option>{/if}
  {#each Object.entries(options) as [value, label] (value)}
    <option {value} selected={value === current}>{label}</option>
  {/each}
{/snippet}

<!-- Escape comes from its fields. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
{#snippet form()}
  <form
    {id}
    class="card"
    inert={busy}
    oninput={() => (changed = !!title)}
    onreset={() => ((changed = false), reset())}
    onkeydown={(event) => title && event.key === "Escape" && event.currentTarget.reset()}
    onsubmit={(event) => {
      event.preventDefault();
      if (title) apply(event.currentTarget, `${title} saved.`);
    }}
    onchange={(event) => {
      const control = event.target as HTMLInputElement;
      // A typed field saves only what it takes: an empty number or one out of range says so instead.
      if (title || !control.reportValidity()) return;
      apply(event.currentTarget, changedTo(control), control.name);
    }}
  >
    {@render children()}
  </form>
{/snippet}

{#if title}
  <div>
    <h3 class="flex h-7 items-center gap-1">
      {title}
      <button type="reset" form={id} class="btn-icon ml-auto size-7" disabled={!changed} title="Cancel (Esc)">
        <X class="size-4" />
      </button>
      <button form={id} class="btn btn-primary size-7 px-0" disabled={!changed || busy} title="Save (Enter)">
        {#if busy && changed}<LoaderCircle class="size-4 animate-spin" />{:else}<Check class="size-4" />{/if}
      </button>
    </h3>
    {@render form()}
  </div>
{:else}
  {@render form()}
{/if}
