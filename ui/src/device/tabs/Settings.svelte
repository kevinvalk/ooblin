<!-- How the device is set up. A switch or choice of its own saves as it changes, fields that go together save together
     from their title's Save and Cancel (or Enter and Escape). What can cut us off or lose something asks first. The
     footer says how it went, the page then shows what the device took. -->
<script lang="ts">
  import { Check, Download, LoaderCircle, Plus, Trash2, X } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { tick, type Snippet } from "svelte";
  import type {
    Availability,
    Certificate,
    Configuration,
    Consent,
    DnsUpdate,
    General,
    Ipv4,
    Ipv6Configuration,
    Ipv6Settings,
    Settings,
  } from "../../bindings/core";
  import type { Change, Changed } from "../../bindings/ooblin";
  import { reason, type Connection } from "../Device.svelte";
  import { CONFIGURATIONS } from "./Status.svelte";

  let {
    data,
    connection,
    say,
    ask,
    saved,
  }: {
    data?: Settings;
    connection: Connection;
    say: (text: string, failed?: boolean) => void;
    ask: (question: string, answers: string[], safe?: boolean) => Promise<string>;
    saved: (data: Settings) => void;
  } = $props();
  /** Ours, as another device's settings may be open too. */
  const id = $props.id();

  let saving = $state(false);
  /** The forms of fields that go together with changes of their own. */
  let changed = $state<Record<string, boolean>>({});

  /** Makes a `change` and shows the settings after, read again however it went: one that failed may have made some
   * of it. Says whether it went through. */
  async function save(done: string, change: Change) {
    saving = true;
    try {
      const { error, settings } = await invoke<Changed>("change", { host: connection.host, change });
      if ("Ok" in settings) saved(settings.Ok);
      const unread = "Err" in settings ? ` The settings couldn't be read again: ${reason(settings.Err)}` : "";
      say((error ? reason(error) : done) + unread, !!(error || unread));
      return !error;
    } catch (error) {
      say(reason(error), true);
      return false;
    } finally {
      saving = false;
    }
  }

  /** Saves a form, read from its fields, then shows what the device took: the settings are the fields' defaults. One
   * that `keep`s what was typed does unless it went through, to try again or cancel. */
  async function apply(form: HTMLFormElement, change: (fields: FormData) => Promise<unknown>, keep = false) {
    if (!(await change(new FormData(form))) && keep) return;
    await tick();
    form.reset();
  }

  /** What a control just changed to, in words: "KVM turned off.", "User consent set to For KVM.". */
  function changedTo(control: HTMLInputElement | HTMLSelectElement) {
    const label = control.labels?.[0].textContent;
    if (control instanceof HTMLSelectElement) return `${label} set to ${control.selectedOptions[0].text}.`;
    return `${label} turned ${control.checked ? "on" : "off"}.`;
  }

  /** A text field's value, trimmed. */
  const text = (fields: FormData, name: string) => ((fields.get(name) as string | null) ?? "").trim();

  async function remoteAccess(fields: FormData, done: string) {
    if (!data) return;
    const was = data.remote_access;
    const access = {
      ...was,
      kvm: was.kvm === null ? null : fields.has("kvm"),
      sol: fields.has("sol"),
      ider: fields.has("ider"),
      consent: (fields.get("consent") as Consent | null) ?? was.consent,
    };
    return save(done, { RemoteAccess: access });
  }

  async function recovery(fields: FormData, done: string) {
    if (!data) return;
    const was = data.recovery;
    const recovery = {
      one_click: was.one_click === null ? null : fields.has("one_click"),
      platform_erase: was.platform_erase === null ? null : fields.has("platform_erase"),
    };
    return save(done, { Recovery: recovery });
  }

  /** The name, ping and how long AMT stays awake are one setting in AMT, which `change` changes some of. */
  async function general(change: Partial<General>, done: string) {
    if (!data) return;
    return save(done, { General: { ...data.general, ...change } });
  }

  const WHEN = [
    ["on", "On"],
    ["asleep", "Asleep or off"],
    ["on_battery", "On battery"],
    ["asleep_on_battery", "Asleep or off on battery"],
  ] as const;

  async function availability(fields: FormData) {
    if (!data) return;
    const was = data.network.available;
    const available = Object.fromEntries(WHEN.map(([when]) => [when, fields.has(when)])) as Availability;
    const lost = WHEN.some(([when]) => was[when] && !available[when]);
    const question = "AMT goes off the network then, and can't be reached until it's back on.";
    if (lost && !(await ask(question, ["Turn off"]))) return;
    return save("Reachability saved.", { Availability: available });
  }

  /** The IP forms' configurations while they're edited: only a manual one takes addresses. */
  let ipv4Configuration = $derived(data?.network.ipv4.configuration);
  let ipv6Configuration = $derived(data?.network.ipv6.configuration);

  /** The addresses an IP form sets, IPv6 all but the subnet mask. */
  const ADDRESSES = [
    ["address", "Address"],
    ["subnet_mask", "Subnet mask"],
    ["gateway", "Gateway"],
    ["primary_dns", "Preferred DNS"],
    ["secondary_dns", "Alternate DNS"],
  ] as const;

  async function ipv4(fields: FormData, done: string) {
    const addresses = Object.fromEntries(ADDRESSES.map(([name]) => [name, text(fields, `ipv4_${name}`)]));
    const ipv4 = { configuration: fields.get("ipv4") as Configuration, ...addresses } as Ipv4;
    const question = `AMT may get another address, which ${connection.host} doesn't reach.`;
    if (await ask(question, ["Save"])) return save(done, { Ipv4: ipv4 });
  }

  async function ipv6(fields: FormData, done: string) {
    const addresses = Object.fromEntries(ADDRESSES.map(([name]) => [name, text(fields, `ipv6_${name}`)]));
    const ipv6 = { configuration: fields.get("ipv6") as Ipv6Configuration, ...addresses } as Ipv6Settings;
    return save(done, { Ipv6: ipv6 });
  }

  async function addCertificate() {
    const extensions = ["pem", "crt", "cer", "der"];
    const path = await invoke<string | null>("pick", { kind: "Certificate", extensions });
    if (!path) return;
    const file = path.split(/[\\/]/).pop();
    const question = `Add ${file} as a certificate, or as a trusted root: AMT then trusts the certificates it signs?`;
    const answer = await ask(question, ["Trusted root", "Certificate"], true);
    if (answer) await save(`${file} added.`, { AddCertificate: { path, trusted_root: answer === "Trusted root" } });
  }

  async function saveCertificate(certificate: Certificate) {
    const path = await invoke<string | null>("pick_save", { name: `${certificate.name}.pem` });
    if (!path) return;
    try {
      await invoke("save_certificate", { host: connection.host, id: certificate.id, path });
      say(`${certificate.name} saved.`);
    } catch (error) {
      say(reason(error), true);
    }
  }

  async function deleteCertificate(certificate: Certificate) {
    if (await ask(`Delete ${certificate.name} from ${connection.info.name}?`, ["Delete"])) {
      await save(`${certificate.name} deleted.`, { DeleteCertificate: certificate.id });
    }
  }

  let alarmDialog: HTMLDialogElement;
  const EVERY: Record<string, string> = { 0: "Not again", 60: "Every hour", 1440: "Every day", 10080: "Every week" };
  const every = (minutes: number) => EVERY[minutes]?.toLowerCase() ?? `every ${minutes} minutes`;

  /** A new alarm starts out tomorrow, at this hour. */
  function newAlarm() {
    const form = alarmDialog.querySelector("form")!;
    form.reset();
    const tomorrow = new Date(Date.now() + 86_400_000);
    tomorrow.setMinutes(0, 0, 0);
    // The field wants local time without a zone, which an ISO string shifted by the zone's offset is.
    tomorrow.setMinutes(-tomorrow.getTimezoneOffset());
    (form.elements.namedItem("at") as HTMLInputElement).value = tomorrow.toISOString().slice(0, 16);
    alarmDialog.showModal();
  }

  /** One that goes off once AMT forgets after. */
  async function addAlarm(event: SubmitEvent) {
    const fields = new FormData(event.currentTarget as HTMLFormElement);
    const name = text(fields, "name");
    const every_minutes = Number(fields.get("every"));
    const alarm = { name, at: Math.floor(new Date(text(fields, "at")).getTime() / 1000), every_minutes, forget: !every_minutes };
    await save(`Wake-up ${name} scheduled.`, { AddAlarm: alarm });
  }

  async function deleteAlarm(name: string) {
    if (await ask(`Cancel the wake-up ${name}?`, ["Cancel it"])) {
      await save(`Wake-up ${name} canceled.`, { DeleteAlarm: name });
    }
  }

  /** The device's clock, ticking on from when we read it. */
  let now = $state(Date.now());
  $effect(() => {
    const ticking = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(ticking);
  });

  /** How far off the device's clock is, in the largest unit there are at least two of. */
  function offBy(offset: number) {
    const off = Math.abs(offset);
    const units = { day: 86400, hour: 3600, minute: 60, second: 1 };
    const [unit, seconds] = Object.entries(units).find(([, seconds]) => off >= 2 * seconds) ?? [];
    if (!unit) return "as on this computer";
    const amount = new Intl.NumberFormat("en-US", { style: "unit", unit, unitDisplay: "long", maximumFractionDigits: 0 });
    return `${amount.format(off / seconds!)} ${offset > 0 ? "ahead of" : "behind"} this computer`;
  }

  const yes = (on?: boolean) => (on === undefined ? undefined : on ? "Yes" : "No");
</script>

<!-- Saves as soon as something changes: `change` gets the form's fields, what changed in words, and its name. -->
{#snippet live(change: (fields: FormData, done: string, name: string) => Promise<unknown>, rows: Snippet)}
  <form
    class="card"
    inert={!data || saving}
    onsubmit={(event) => event.preventDefault()}
    onchange={(event) => {
      const control = event.target as HTMLInputElement;
      const done = changedTo(control);
      apply(event.currentTarget, (fields) => change(fields, done, control.name));
    }}
  >
    {@render rows()}
  </form>
{/snippet}

<!-- Saves together from its title's Save and Cancel, there all along so it's clear what saves this way. -->
{#snippet grouped(
  form: string,
  title: string,
  change: (fields: FormData, done: string) => Promise<unknown>,
  rows: Snippet,
  reset = () => {},
)}
  <div>
    <h3 class="flex items-center gap-1">
      {title}
      <button
        type="reset"
        form="{id}-{form}"
        class="btn-icon -my-1 ml-auto size-7"
        disabled={!changed[form]}
        title="Cancel (Esc)"
      >
        <X class="size-4" />
      </button>
      <button
        form="{id}-{form}"
        class="btn btn-primary -my-1 size-7 px-0"
        disabled={!changed[form] || saving}
        title="Save (Enter)"
      >
        {#if saving && changed[form]}<LoaderCircle class="size-4 animate-spin" />{:else}<Check class="size-4" />{/if}
      </button>
    </h3>
    <!-- Escape comes from its fields. -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <form
      id="{id}-{form}"
      class="card"
      inert={!data || saving}
      oninput={() => (changed[form] = true)}
      onreset={() => ((changed[form] = false), reset())}
      onkeydown={(event) => event.key === "Escape" && event.currentTarget.reset()}
      onsubmit={(event) => {
        event.preventDefault();
        apply(event.currentTarget, (fields) => change(fields, `${title} saved.`), true);
      }}
    >
      {@render rows()}
    </form>
  </div>
{/snippet}

<!-- A switch, which can't be switched where the device has none (`null`). -->
{#snippet check(name: string, label: string, checked: boolean | null | undefined, help?: string)}
  <div title={checked === null ? "This device can't." : help}>
    <label for="{id}-{name}">{label}</label>
    <input id="{id}-{name}" {name} type="checkbox" defaultChecked={checked} disabled={checked === null} />
  </div>
{/snippet}

<!-- A select's options, the device's `current` one selected: unlike the select's own default, a selected option counts
     when it comes in after the select, the settings being read. -->
{#snippet choices(current: string | undefined, options: Record<string, string>)}
  {#each Object.entries(options) as [value, label] (value)}
    <option {value} selected={value === current}>{label}</option>
  {/each}
{/snippet}

<!-- An address field, which only a manual configuration takes. -->
{#snippet address(name: string, label: string, value: string | undefined, manual: boolean)}
  <div>
    <label for="{id}-{name}">{label}</label>
    <input
      id="{id}-{name}"
      {name}
      class="field font-mono"
      autocapitalize="off"
      spellcheck="false"
      defaultValue={value}
      disabled={!manual}
      required={manual && (name === "ipv4_address" || name === "ipv4_subnet_mask")}
    />
  </div>
{/snippet}

<section>
  <h2>Remote access</h2>
  {#snippet rows()}
    {@const access = data?.remote_access}
    {@const recovery = data?.recovery}
    {@render check("kvm", "KVM", access?.kvm, "Seeing and using the device's screen.")}
    {@render check("sol", "Serial console", access?.sol, "Its serial port, for consoles that write to one.")}
    {@render check("ider", "Disk images", access?.ider, "Booting it from a disk image on this computer (IDE redirection).")}
    <div title={access?.consent_fixed ? "In client control mode it's always asked for." : "Someone at the device reads out a code first."}>
      <label for="{id}-consent">User consent</label>
      <select id="{id}-consent" name="consent" class="field" disabled={access?.consent_fixed}>
        {@render choices(access?.consent, { None: "Not required", Kvm: "For KVM", All: "For all redirection" })}
      </select>
    </div>
    {@render check("one_click", "One-Click Recovery", recovery?.one_click, "Booting a recovery image, from the network or its disk.")}
    {@render check("platform_erase", "Remote platform erase", recovery?.platform_erase, "Erasing its disks and settings from here.")}
  {/snippet}
  {@render live(
    (fields, done, name) => (name === "one_click" || name === "platform_erase" ? recovery : remoteAccess)(fields, done),
    rows,
  )}
</section>

<section>
  <h2>Network</h2>
  <div class="cards">
    {#snippet reachable()}
      {@render check("ping", "Respond to ping", data?.general.respond_to_ping)}
      <div title="When AMT is on the network: with the device on, or asleep or off, and on mains power or a battery.">
        <span class="text-muted">Reachable</span>
        <span class="flex flex-wrap gap-x-5 gap-y-1">
          {#each WHEN as [when, label] (when)}
            <label class="flex items-center gap-2">
              <input name={when} type="checkbox" defaultChecked={data?.network.available[when]} />
              {label}
            </label>
          {/each}
        </span>
      </div>
    {/snippet}
    <div class="@3xl:col-span-2">
      <h3>Reachability</h3>
      {@render live(
        (fields, done, name) =>
          name === "ping" ? general({ respond_to_ping: fields.has("ping") }, done) : availability(fields),
        reachable,
      )}
    </div>

    {#snippet naming()}
      <div title="The host name, with its domain if it has one.">
        <label for="{id}-name">Host name</label>
        <input
          id="{id}-name"
          name="name"
          class="field"
          autocapitalize="off"
          spellcheck="false"
          maxlength="255"
          required
          defaultValue={data?.general.name}
        />
      </div>
      {@render check("shared", "Shared with the OS", data?.general.shared_with_os, "Whether the OS goes by the same name.")}
      <div title="Who puts AMT's name and address in DNS: the DHCP server when AMT asks it to, or AMT itself (DDNS).">
        <label for="{id}-dns">Registered in DNS by</label>
        <select id="{id}-dns" name="dns" class="field">
          {@render choices(data?.general.dns_update, { Off: "Nobody", Dhcp: "The DHCP server", Amt: "AMT itself" })}
        </select>
      </div>
    {/snippet}
    <div class="@3xl:col-span-2">
      {@render grouped(
        "name-form",
        "Name",
        (fields, done) =>
          general(
            { name: text(fields, "name"), shared_with_os: fields.has("shared"), dns_update: fields.get("dns") as DnsUpdate },
            done,
          ),
        naming,
      )}
    </div>

    {#snippet v4()}
      {@const ipv4 = data?.network.ipv4}
      <div>
        <label for="{id}-ipv4">Configuration</label>
        <select
          id="{id}-ipv4"
          name="ipv4"
          class="field"
          oninput={(event) => (ipv4Configuration = event.currentTarget.value as Configuration)}
        >
          {@render choices(ipv4?.configuration, CONFIGURATIONS)}
        </select>
      </div>
      {#each ADDRESSES as [name, label] (name)}
        {@render address(`ipv4_${name}`, label, ipv4?.[name], ipv4Configuration === "Manual")}
      {/each}
    {/snippet}
    {@render grouped("ipv4-form", "IPv4", ipv4, v4, () => (ipv4Configuration = data?.network.ipv4.configuration))}

    {#snippet v6()}
      {@const ipv6 = data?.network.ipv6}
      <div>
        <label for="{id}-ipv6">Configuration</label>
        <select
          id="{id}-ipv6"
          name="ipv6"
          class="field"
          oninput={(event) => (ipv6Configuration = event.currentTarget.value as Ipv6Configuration)}
        >
          {@render choices(ipv6?.configuration, { Off: "Off", Automatic: "Automatic", Manual: "Automatic and manual" })}
        </select>
      </div>
      {#each ADDRESSES as [name, label] (name)}
        {#if name !== "subnet_mask"}
          {@render address(`ipv6_${name}`, label, ipv6?.[name], ipv6Configuration === "Manual")}
        {/if}
      {/each}
    {/snippet}
    {@render grouped("ipv6-form", "IPv6", ipv6, v6, () => (ipv6Configuration = data?.network.ipv6.configuration))}
  </div>
</section>

<section>
  <h2>Clock and power</h2>
  {#snippet power()}
    <div title="When AMT itself is powered, and so reachable: the device's maker offers these.">
      <label for="{id}-power_policy">Power policy</label>
      <select id="{id}-power_policy" name="power_policy" class="field" disabled={data && data.power_policies.length < 2}>
        {@render choices(
          data?.power_policies.find((policy) => policy.active)?.id,
          Object.fromEntries(data?.power_policies.map((policy) => [policy.id, policy.description]) ?? []),
        )}
      </select>
    </div>
    <div title="How long AMT stays powered after the device sleeps, or after it woke the device for us, in minutes.">
      <label for="{id}-awake">Awake after</label>
      <span class="flex items-center gap-2">
        <input
          id="{id}-awake"
          name="awake"
          type="number"
          class="field w-24"
          min="1"
          max="65535"
          required
          defaultValue={data?.general.awake_minutes}
        />
        minutes
      </span>
    </div>
  {/snippet}
  <div class="grid gap-x-4 gap-y-6">
    <div>
      <h3>Clock</h3>
      <dl class="card">
        <div>
          <dt>Time</dt>
          <dd class="flex items-center justify-between gap-4">
            <span class="tabular-nums">
              {data?.clock_offset != null
                ? `${new Date(now + data.clock_offset * 1000).toLocaleString()}, ${offBy(data.clock_offset)}`
                : ""}
            </span>
            <button
              type="button"
              class="btn btn-secondary -my-1 h-7"
              disabled={!data || saving}
              title="Sets the device's clock to this computer's."
              onclick={() => save("Clock set to this computer's.", "Clock")}
            >
              Set to this computer's
            </button>
          </dd>
        </div>
      </dl>
    </div>

    <div>
      <h3>Power</h3>
      {@render live(
        (fields, done, name) =>
          name === "awake"
            ? general({ awake_minutes: Number(fields.get("awake")) }, `AMT stays awake ${fields.get("awake")} minutes.`)
            : save(done, { PowerPolicy: fields.get("power_policy") as string }),
        power,
      )}
    </div>

    <div title="AMT turns the device on at these times, when the power policy keeps AMT itself powered.">
      <h3 class="flex items-center">
        Scheduled wake-ups
        <button type="button" class="btn btn-secondary -my-1 ml-auto h-7" disabled={!data || saving} onclick={newAlarm}>
          <Plus class="size-4" /> Schedule…
        </button>
      </h3>
      <dl class="card" inert={saving}>
        {#each data?.alarms ?? [undefined] as alarm (alarm?.name)}
          <div>
            <dt>{alarm?.name}</dt>
            <dd class="flex items-center gap-1">
              {#if alarm}
                <span class="mr-auto">
                  {new Date(alarm.at * 1000).toLocaleString() + (alarm.every_minutes ? `, then ${every(alarm.every_minutes)}` : "")}
                </span>
                <button type="button" class="btn-icon -my-1 size-7" title="Cancel…" onclick={() => deleteAlarm(alarm.name)}>
                  <Trash2 class="size-4" />
                </button>
              {/if}
            </dd>
          </div>
        {:else}
          <p class="px-4 py-2.5 text-muted">None, the device only wakes up when asked to.</p>
        {/each}
      </dl>
    </div>
  </div>
</section>

<section>
  <h2>Security</h2>
  <div class="cards">
    {#each data?.tls ?? [undefined] as tls (tls?.local)}
      <div title="As AMT was set up: changing it would lock us out, or isn't supported.">
        <h3>{tls && (tls.local ? "TLS on the device" : "TLS on the network")}</h3>
        <dl class="card">
          <div><dt>On</dt><dd>{yes(tls?.enabled)}</dd></div>
          <div><dt>Mutual</dt><dd>{yes(tls?.mutual)}</dd></div>
          <div><dt>Without TLS too</dt><dd>{tls && (tls.plain_supported ? yes(tls.plain_too) : "Not supported")}</dd></div>
        </dl>
      </div>
    {/each}

    <div class="@3xl:col-span-2">
      <h3 class="flex items-center">
        Certificates
        <button
          type="button"
          class="btn btn-secondary -my-1 ml-auto h-7"
          disabled={!data || saving}
          onclick={addCertificate}
        >
          <Plus class="size-4" /> Add…
        </button>
      </h3>
      <dl class="card" inert={saving}>
        {#each data?.certificates ?? [undefined] as certificate (certificate?.id)}
          <div title={certificate && `Issued by ${certificate.issuer}`}>
            <dt>{certificate?.name}</dt>
            <dd class="flex items-center gap-1">
              {#if certificate}
                <span class="mr-auto">
                  {(certificate.trusted_root ? "Trusted root" : certificate.read_only ? "AMT's own" : "Certificate") +
                    (certificate.in_use ? ", used by TLS" : "")}
                </span>
                <button
                  type="button"
                  class="btn-icon -my-1 size-7"
                  title="Save…"
                  onclick={() => saveCertificate(certificate)}
                >
                  <Download class="size-4" />
                </button>
                <button
                  type="button"
                  class="btn-icon -my-1 size-7"
                  disabled={certificate.read_only || certificate.in_use}
                  title={certificate.read_only || certificate.in_use ? "AMT keeps this one." : "Delete…"}
                  onclick={() => deleteCertificate(certificate)}
                >
                  <Trash2 class="size-4" />
                </button>
              {/if}
            </dd>
          </div>
        {/each}
      </dl>
    </div>
  </div>
</section>

<p class="text-center text-muted">
  Users, and what isn't here, are in
  <button
    type="button"
    class="text-accent hover:underline"
    onclick={() => invoke("web_pages", { host: connection.host }).catch((error) => say(reason(error), true))}
  >
    AMT's own web pages</button
  >.
</p>

<dialog bind:this={alarmDialog}>
  <form method="dialog" class="grid gap-4" onsubmit={addAlarm}>
    <p class="font-medium">Schedule a wake-up</p>
    <p class="text-muted">AMT turns {connection.info.name} on then, or wakes it from sleep.</p>
    <label class="grid gap-1.5">
      Name
      <input name="name" class="field" autocapitalize="off" spellcheck="false" maxlength="32" required />
    </label>
    <label class="grid gap-1.5">At <input name="at" type="datetime-local" class="field" required /></label>
    <label class="grid gap-1.5">
      Then
      <select name="every" class="field">
        {#each Object.entries(EVERY) as [minutes, label] (minutes)}<option value={minutes}>{label}</option>{/each}
      </select>
    </label>
    <div class="flex justify-end gap-2">
      <button type="button" class="btn btn-secondary" onclick={() => alarmDialog.close()}>Cancel</button>
      <button class="btn btn-primary">Schedule</button>
    </div>
  </form>
</dialog>
