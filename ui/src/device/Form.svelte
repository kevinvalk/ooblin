<script module lang="ts">
  /** The options of a select, with the `current` one of the device selected: unlike the default of the select itself, a
   * selected option counts when it comes in after the select, once the settings are read. A `current` that is none of
   * them shows as it is, so that saving what is around it does not change it. */
  export { choices };

  /** A choice of `options`, with `help` saying what it is, which cannot change where a `loaded` BMC offers none. Its
   * `id` is the `$props.id()` of its page. */
  export { select };

  /** The value of a text field, trimmed. */
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
    /** With a `title` the fields go together and save together, from the ✓ and ✕ of the title (or Enter and Escape).
     * Those are there all along so that it is clear what saves this way, and enabled once something changed. Without a
     * title each switch or choice saves as it changes. */
    title?: string;

    busy: boolean; // While the settings are read or anything saves

    /** Gets the fields of the form, what changed in words ("IPv4 saved.", "KVM turned off.") and the name of the
     * changed control. When it says the change did not go through, a titled form keeps what was typed, to try again or
     * cancel. Otherwise the form then shows what the device took, as the settings are the defaults of the fields. */
    save: (fields: FormData, done: string, name: string) => Promise<unknown>;

    reset?: () => void;
    children: Snippet;
  } = $props();
  const id = $props.id();
  let isChanged = $state(false);

  /** Saves, then shows what the device took, unless what was typed is kept. */
  async function apply(form: HTMLFormElement, done: string, name = "") {
    if (!(await save(new FormData(form), done, name)) && title !== undefined) return;

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

{#snippet select(
  id: string,
  name: string,
  label: string,
  current: string | undefined,
  options: Record<string, string>,
  help: string,
  loaded: boolean,
)}
  {@const hasNone = loaded && Object.keys(options).length <= 0}
  <div title={hasNone ? "This BMC can't." : help}>
    <label for="{id}-{name}">{label}</label>
    <select id="{id}-{name}" {name} class="field" disabled={hasNone}>
      {@render choices(current, options)}
    </select>
  </div>
{/snippet}

<!-- Escape comes from its fields. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
{#snippet form()}
  <form
    {id}
    class="card"
    inert={busy}
    oninput={() => (isChanged = title !== undefined)}
    onreset={() => ((isChanged = false), reset())}
    onkeydown={(event) => title !== undefined && event.key === "Escape" && event.currentTarget.reset()}
    onsubmit={(event) => {
      event.preventDefault();
      if (title !== undefined) apply(event.currentTarget, `${title} saved.`);
    }}
    onchange={(event) => {
      const control = event.target as HTMLInputElement;

      // A typed field saves only what it takes: an empty number or one out of range says so instead.
      if (title !== undefined || !control.reportValidity()) return;

      apply(event.currentTarget, changedTo(control), control.name);
    }}
  >
    {@render children()}
  </form>
{/snippet}

{#if title !== undefined}
  <div>
    <h3 class="flex h-7 items-center gap-1">
      {title}
      <button type="reset" form={id} class="btn-icon ml-auto size-7" disabled={!isChanged} title="Cancel (Esc)">
        <X class="size-4" />
      </button>
      <button form={id} class="btn btn-primary size-7 px-0" disabled={!isChanged || busy} title="Save (Enter)">
        {#if busy && isChanged}<LoaderCircle class="size-4 animate-spin" />{:else}<Check class="size-4" />{/if}
      </button>
    </h3>
    {@render form()}
  </div>
{:else}
  {@render form()}
{/if}
