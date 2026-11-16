<script lang="ts">
  import { Download, Plus, Trash2 } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type {
    Availability,
    Certificate,
    Change,
    Configuration,
    Consent,
    DnsUpdate,
    General,
    Ipv4,
    Ipv6Configuration,
    Ipv6Settings,
    Settings,
  } from "../../bindings/core";
  import { reason, type Connection } from "../../connection";
  import Form, { choices, text } from "../Form.svelte";
  import { CONFIGURATIONS } from "../Network.svelte";

  let {
    data,
    connection,
    title,
    say,
    ask,
    save,
    busy,
  }: {
    data?: Settings;
    connection: Connection;
    title: string; // Its name, or the address we reach it at
    say: (text: string, isFailed?: boolean) => void;
    ask: (question: string, answers: string[], isSafe?: boolean) => Promise<string>;
    save: (done: string, change: Change) => Promise<boolean>;
    busy: boolean;
  } = $props();
  const id = $props.id(); // Our own, as the settings of another device may be open too

  async function remoteAccess(fields: FormData, done: string) {
    if (data === undefined) return;

    const was = data.remote_access;
    const access = {
      ...was,
      is_kvm: was.is_kvm === null ? null : fields.has("kvm"),
      is_sol: fields.has("sol"),
      is_ider: fields.has("ider"),
      consent: (fields.get("consent") as Consent | null) ?? was.consent,
    };
    return save(done, { RemoteAccess: access });
  }

  async function recovery(fields: FormData, done: string) {
    if (data === undefined) return;

    const was = data.recovery;
    const recovery = {
      is_one_click: was.is_one_click === null ? null : fields.has("one_click"),
      is_platform_erase: was.is_platform_erase === null ? null : fields.has("platform_erase"),
    };
    return save(done, { Recovery: recovery });
  }

  /** The name, ping and how long AMT stays awake are one setting in AMT, which `change` changes some of. */
  async function general(change: Partial<General>, done: string) {
    if (data === undefined) return;

    return save(done, { General: { ...data.general, ...change } });
  }

  const WHEN = [
    ["is_on", "On"],
    ["is_asleep", "Asleep or off"],
    ["is_on_battery", "On battery"],
    ["is_asleep_on_battery", "Asleep or off on battery"],
  ] as const;

  async function availability(fields: FormData) {
    if (data === undefined) return;

    const was = data.network.available;
    const available = Object.fromEntries(WHEN.map(([when]) => [when, fields.has(when)])) as Availability;
    const isLost = WHEN.some(([when]) => was[when] && !available[when]);
    const question = "AMT goes off the network then, and can't be reached until it's back on.";
    if (isLost && (await ask(question, ["Turn off"])) === "") return;

    return save("Reachability saved.", { Availability: available });
  }

  /** The configurations of the IP forms while they are `edited`, as only a manual one takes addresses. An edit outlasts
   * the settings, which land again after every save, one that failed too. */
  let edited = $state<{ ipv4?: Configuration; ipv6?: Ipv6Configuration }>({});
  let ipv4Configuration = $derived(edited.ipv4 ?? data?.network.ipv4.configuration);
  let ipv6Configuration = $derived(edited.ipv6 ?? data?.network.ipv6.configuration);

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
    if ((await ask(question, ["Save"])) !== "") return save(done, { Ipv4: ipv4 });
  }

  async function ipv6(fields: FormData, done: string) {
    const addresses = Object.fromEntries(ADDRESSES.map(([name]) => [name, text(fields, `ipv6_${name}`)]));
    const ipv6 = { configuration: fields.get("ipv6") as Ipv6Configuration, ...addresses } as Ipv6Settings;
    return save(done, { Ipv6: ipv6 });
  }

  async function addCertificate() {
    const extensions = ["pem", "crt", "cer", "der"];
    const path = await invoke<string | null>("pick", { kind: "Certificate", extensions });
    if (path === null) return;

    const fileName = path.split(/[\\/]/).pop();
    const question = `Add ${fileName} as a certificate, or as a trusted root: AMT then trusts the certificates it signs?`;
    const answer = await ask(question, ["Trusted root", "Certificate"], true);
    if (answer !== "") await save(`${fileName} added.`, { AddCertificate: { path, is_trusted_root: answer === "Trusted root" } });
  }

  async function saveCertificate(certificate: Certificate) {
    const path = await invoke<string | null>("pick_save", { name: `${certificate.name}.pem` });
    if (path === null) return;

    try {
      await invoke("save_certificate", { host: connection.host, id: certificate.id, path });
      say(`${certificate.name} saved.`);
    } catch (error) {
      say(reason(error), true);
    }
  }

  async function deleteCertificate(certificate: Certificate) {
    if ((await ask(`Delete ${certificate.name} from ${title}?`, ["Delete"])) !== "") {
      await save(`${certificate.name} deleted.`, { DeleteCertificate: certificate.id });
    }
  }

  let alarmDialogElement: HTMLDialogElement;
  const EVERY: Record<string, string> = { 0: "Not again", 60: "Every hour", 1440: "Every day", 10080: "Every week" };
  const every = (minutes: number) => EVERY[minutes]?.toLowerCase() ?? `every ${minutes} minutes`;

  /** A new alarm starts out tomorrow, at this hour. */
  function newAlarm() {
    const form = alarmDialogElement.querySelector("form")!;
    form.reset();

    const tomorrow = new Date(Date.now() + 86_400_000);
    tomorrow.setMinutes(0, 0, 0);
    // The field wants local time without a zone, which an ISO string shifted by the offset of the zone is.
    tomorrow.setMinutes(-tomorrow.getTimezoneOffset());
    (form.elements.namedItem("at") as HTMLInputElement).value = tomorrow.toISOString().slice(0, 16);

    alarmDialogElement.showModal();
  }

  /** AMT forgets an alarm that goes off only once after it did. */
  async function addAlarm(event: SubmitEvent) {
    // We close the dialog ourselves and only once the alarm is saved, else a failed save loses what was typed.
    event.preventDefault();

    const fields = new FormData(event.currentTarget as HTMLFormElement);
    const name = text(fields, "name");
    const every_minutes = Number(fields.get("every"));
    const alarm = { name, next_at: Math.floor(new Date(text(fields, "at")).getTime() / 1000), every_minutes, is_forget: every_minutes === 0 };
    if (await save(`Wake-up ${name} scheduled.`, { AddAlarm: alarm })) alarmDialogElement.close();
  }

  async function deleteAlarm(name: string) {
    if ((await ask(`Cancel the wake-up ${name}?`, ["Cancel it"])) !== "") {
      await save(`Wake-up ${name} canceled.`, { DeleteAlarm: name });
    }
  }

  /** The clock of the device, ticking on from when we read it. */
  let now = $state(Date.now());
  $effect(() => {
    if (data?.clock_offset == null) return;

    const ticking = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(ticking);
  });

  /** How far off the clock of the device is, in the largest unit there are at least two of. */
  function offBy(offset: number) {
    const distance = Math.abs(offset);
    const units = { day: 86400, hour: 3600, minute: 60, second: 1 };
    const [unit, seconds] = Object.entries(units).find(([, seconds]) => distance >= 2 * seconds) ?? [];
    if (unit === undefined) return "as on this computer";

    const amount = new Intl.NumberFormat("en-US", { style: "unit", unit, unitDisplay: "long", maximumFractionDigits: 0 });
    return `${amount.format(distance / seconds!)} ${offset > 0 ? "ahead of" : "behind"} this computer`;
  }

  const yes = (on?: boolean) => (on === undefined ? undefined : on ? "Yes" : "No");
</script>

<!-- A switch, which cannot be switched where the device has none (`null`). -->
{#snippet check(name: string, label: string, checked: boolean | null | undefined, help?: string)}
  <div title={checked === null ? "This device can't." : help}>
    <label for="{id}-{name}">{label}</label>
    <input id="{id}-{name}" {name} type="checkbox" defaultChecked={checked} disabled={checked === null} />
  </div>
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
  <Form
    {busy}
    save={(fields, done, name) =>
      (name === "one_click" || name === "platform_erase" ? recovery : remoteAccess)(fields, done)}
  >
    {@const access = data?.remote_access}
    {@const recoveries = data?.recovery}
    {@render check("kvm", "KVM", access?.is_kvm, "Seeing and using the device's screen.")}
    {@render check("sol", "Serial console", access?.is_sol, "Its serial port, for consoles that write to one.")}
    {@render check("ider", "Disk images", access?.is_ider, "Booting it from a disk image on this computer (IDE redirection).")}
    <div title={access?.is_consent_fixed ? "In client control mode it's always asked for." : "Someone at the device reads out a code first."}>
      <label for="{id}-consent">User consent</label>
      <select id="{id}-consent" name="consent" class="field" disabled={access?.is_consent_fixed}>
        {@render choices(access?.consent, { None: "Not required", Kvm: "For KVM", All: "For all redirection" })}
      </select>
    </div>
    {@render check("one_click", "One-Click Recovery", recoveries?.is_one_click, "Booting a recovery image, from the network or its disk.")}
    {@render check("platform_erase", "Remote platform erase", recoveries?.is_platform_erase, "Erasing its disks and settings from here.")}
  </Form>
</section>

<section>
  <h2>Network</h2>
  <div class="cards">
    <div class="@3xl:col-span-2">
      <h3>Reachability</h3>
      <Form
        {busy}
        save={(fields, done, name) =>
          name === "ping" ? general({ is_respond_to_ping: fields.has("ping") }, done) : availability(fields)}
      >
        {@render check("ping", "Respond to ping", data?.general.is_respond_to_ping)}
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
      </Form>
    </div>

    <div class="@3xl:col-span-2">
      <Form
        title="Name"
        {busy}
        save={(fields, done) =>
          general(
            { name: text(fields, "name"), is_shared_with_os: fields.has("shared"), dns_update: fields.get("dns") as DnsUpdate },
            done,
          )}
      >
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
        {@render check("shared", "Shared with the OS", data?.general.is_shared_with_os, "Whether the OS goes by the same name.")}
        <div title="Who puts AMT's name and address in DNS: the DHCP server when AMT asks it to, or AMT itself (DDNS).">
          <label for="{id}-dns">Registered in DNS by</label>
          <select id="{id}-dns" name="dns" class="field">
            {@render choices(data?.general.dns_update, { Off: "Nobody", Dhcp: "The DHCP server", Amt: "AMT itself" })}
          </select>
        </div>
      </Form>
    </div>

    <Form
      title="IPv4"
      {busy}
      save={ipv4}
      reset={() => (edited.ipv4 = undefined)}
    >
      {@const v4 = data?.network.ipv4}
      <div>
        <label for="{id}-ipv4">Configuration</label>
        <select
          id="{id}-ipv4"
          name="ipv4"
          class="field"
          oninput={(event) => (edited.ipv4 = event.currentTarget.value as Configuration)}
        >
          {@render choices(v4?.configuration, CONFIGURATIONS)}
        </select>
      </div>
      {#each ADDRESSES as [name, label] (name)}
        {@render address(`ipv4_${name}`, label, v4?.[name], ipv4Configuration === "Manual")}
      {/each}
    </Form>

    <Form
      title="IPv6"
      {busy}
      save={ipv6}
      reset={() => (edited.ipv6 = undefined)}
    >
      {@const v6 = data?.network.ipv6}
      <div>
        <label for="{id}-ipv6">Configuration</label>
        <select
          id="{id}-ipv6"
          name="ipv6"
          class="field"
          oninput={(event) => (edited.ipv6 = event.currentTarget.value as Ipv6Configuration)}
        >
          {@render choices(v6?.configuration, { Off: "Off", Automatic: "Automatic", Manual: "Automatic and manual" })}
        </select>
      </div>
      {#each ADDRESSES as [name, label] (name)}
        {#if name !== "subnet_mask"}
          {@render address(`ipv6_${name}`, label, v6?.[name], ipv6Configuration === "Manual")}
        {/if}
      {/each}
    </Form>
  </div>
</section>

<section>
  <h2>Clock and power</h2>
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
              disabled={busy}
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
      <Form
        {busy}
        save={(fields, done, name) =>
          name === "awake"
            ? general({ awake_minutes: Number(fields.get("awake")) }, `AMT stays awake ${fields.get("awake")} minutes.`)
            : save(done, { PowerPolicy: fields.get("power_policy") as string })}
      >
        <div title="When AMT itself is powered, and so reachable: the device's maker offers these.">
          <label for="{id}-power_policy">Power policy</label>
          <select id="{id}-power_policy" name="power_policy" class="field" disabled={data && data.power_policies.length < 2}>
            {@render choices(
              data?.power_policies.find((policy) => policy.is_active)?.id,
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
      </Form>
    </div>

    <div title="AMT turns the device on at these times, when the power policy keeps AMT itself powered.">
      <h3 class="flex h-7 items-center">
        Scheduled wake-ups
        <button type="button" class="btn btn-secondary ml-auto h-7" disabled={busy} onclick={newAlarm}>
          <Plus class="size-4" /> Schedule…
        </button>
      </h3>
      <dl class="card" inert={busy}>
        {#each data?.alarms ?? [undefined] as alarm (alarm?.name)}
          <div>
            <dt>{alarm?.name}</dt>
            <dd class="flex items-center gap-1">
              {#if alarm}
                <span class="mr-auto">
                  {new Date(alarm.next_at * 1000).toLocaleString() + (alarm.every_minutes ? `, then ${every(alarm.every_minutes)}` : "")}
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
    {#each data?.tls ?? [undefined] as tls (tls?.is_local)}
      <div title="As AMT was set up: changing it would lock us out, or isn't supported.">
        <h3>{tls && (tls.is_local ? "TLS on the device" : "TLS on the network")}</h3>
        <dl class="card">
          <div><dt>On</dt><dd>{yes(tls?.is_enabled)}</dd></div>
          <div><dt>Mutual</dt><dd>{yes(tls?.is_mutual)}</dd></div>
          <div><dt>Without TLS too</dt><dd>{tls && (tls.is_plain_supported ? yes(tls.is_plain_too) : "Not supported")}</dd></div>
        </dl>
      </div>
    {/each}

    <div class="@3xl:col-span-2">
      <h3 class="flex h-7 items-center">
        Certificates
        <button
          type="button"
          class="btn btn-secondary ml-auto h-7"
          disabled={busy}
          onclick={addCertificate}
        >
          <Plus class="size-4" /> Add…
        </button>
      </h3>
      <dl class="card" inert={busy}>
        {#each data?.certificates ?? [undefined] as certificate (certificate?.id)}
          <div title={certificate && `Issued by ${certificate.issuer}`}>
            <dt>{certificate?.name}</dt>
            <dd class="flex items-center gap-1">
              {#if certificate}
                <span class="mr-auto">
                  {(certificate.is_trusted_root ? "Trusted root" : certificate.is_read_only ? "AMT's own" : "Certificate") +
                    (certificate.is_in_use ? ", used by TLS" : "")}
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
                  disabled={certificate.is_read_only || certificate.is_in_use}
                  title={certificate.is_read_only || certificate.is_in_use ? "AMT keeps this one." : "Delete…"}
                  onclick={() => deleteCertificate(certificate)}
                >
                  <Trash2 class="size-4" />
                </button>
              {/if}
            </dd>
          </div>
        {:else}
          <p class="px-4 py-2.5 text-muted">None.</p>
        {/each}
      </dl>
    </div>
  </div>
</section>

<dialog bind:this={alarmDialogElement}>
  <form method="dialog" class="grid gap-4" onsubmit={addAlarm}>
    <p class="font-medium">Schedule a wake-up</p>
    <p class="text-muted">AMT turns {title} on then, or wakes it from sleep.</p>
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
      <button type="button" class="btn btn-secondary" onclick={() => alarmDialogElement.close()}>Cancel</button>
      <button class="btn btn-primary">Schedule</button>
    </div>
  </form>
</dialog>
