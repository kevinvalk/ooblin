<!-- How a BMC is set up, in `Form`s. What it serves changes together, as the BMC's own port page does it: IPMI and
     HTTPS, which we speak, only there. The footer says how it went. -->
<script lang="ts">
  import type { BmcChange, BmcSettings } from "../../bindings/core";
  import Form, { select, text } from "../Form.svelte";
  import type { Connection } from "../Device.svelte";
  import Fru from "../ipmi/Fru.svelte";
  import NetworkSettings from "../ipmi/NetworkSettings.svelte";
  import Server from "../ipmi/Server.svelte";

  let {
    data,
    connection,
    save,
    busy,
    ask,
  }: {
    data?: BmcSettings;
    connection: Connection;
    save: (done: string, change: BmcChange) => Promise<boolean>;
    busy: boolean;
    ask: (question: string, answers: string[], safe?: boolean) => Promise<string>;
  } = $props();
  const id = $props.id();

  /** Redfish's names, in words where they aren't, and those we speak, which change only in the BMC's own pages. */
  const SERVICES: Record<string, string> = { KVMIP: "KVM", VirtualMedia: "Virtual media" };
  const OURS = ["IPMI", "HTTPS"];
  const MODES: Record<string, string> = { FullSpeed: "Full speed", HeavyIO: "Heavy IO", PUE2: "Power efficient" };

  /** The services as the form has them, turning the KVM off asking first. */
  async function services(fields: FormData) {
    const services = (data?.services ?? [])
      .filter((service) => !OURS.includes(service.name))
      .map((service) => ({
        name: service.name,
        enabled: fields.has(`${service.name}-on`),
        port: Number(fields.get(`${service.name}-port`)) || service.port,
      }));
    const kvm = services.find((service) => service.name === "KVMIP");
    const question = "The KVM stops working, here too, and virtual media turns off with it.";
    if (kvm && !kvm.enabled && data?.services.find((service) => service.name === "KVMIP")?.enabled) {
      if (!(await ask(question, ["Turn off"]))) return false;
    }
    return save("Services saved.", { Services: services });
  }

  /** A mode's choices, in words. */
  const modes = (modes?: string[]) => Object.fromEntries((modes ?? []).map((mode) => [mode, MODES[mode] ?? mode]));
</script>

<section>
  <h2>Server</h2>
  <Server loaded={!!data} restore={data?.power_restore} sol={data?.sol} {save} {busy} />
</section>

<section>
  <h2>BMC</h2>
  <div class="cards">
    <div class="@3xl:col-span-2">
      <NetworkSettings network={data?.network} host={connection.host} {save} {busy} {ask} />
    </div>
    <div class="@3xl:col-span-2">
      <Form
        {busy}
        save={(fields, done, name) =>
          save(done, name === "fan" ? { FanMode: text(fields, "fan") } : { MouseMode: text(fields, "mouse") })}
      >
        {@render select(
          id,
          "fan",
          "Fan mode",
          data?.fan_mode.mode,
          modes(data?.fan_mode.modes),
          "How fast the BMC runs the fans: Standard and Optimal are quieter, Full speed and Heavy IO cooler.",
          !!data,
        )}
        {@render select(
          id,
          "mouse",
          "KVM mouse mode",
          data?.mouse_mode.mode,
          modes(data?.mouse_mode.modes),
          "We send where the pointer is, which only Absolute takes as it is: in another mode it may not land where clicked.",
          !!data,
        )}
      </Form>
    </div>

    <Form
      title="Time"
      {busy}
      save={(fields, done) =>
        save(done, {
          Ntp: { enabled: fields.has("ntp"), primary: text(fields, "primary"), secondary: text(fields, "secondary") },
        })}
    >
      <div title="The BMC sets its clock from these, its event log's times with it.">
        <label for="{id}-ntp">From NTP</label>
        <input id="{id}-ntp" name="ntp" type="checkbox" defaultChecked={data?.ntp.enabled} />
      </div>
      {#each [["primary", "Server"], ["secondary", "Second server"]] as const as [name, label] (name)}
        <div>
          <label for="{id}-{name}">{label}</label>
          <input
            id="{id}-{name}"
            {name}
            class="field font-mono"
            autocapitalize="off"
            spellcheck="false"
            defaultValue={data?.ntp[name]}
          />
        </div>
      {/each}
    </Form>

    <Form
      title="Syslog"
      {busy}
      save={(fields, done) =>
        save(done, {
          Syslog: { enabled: fields.has("syslog"), server: text(fields, "server"), port: Number(fields.get("port")) },
        })}
    >
      <div title="The BMC sends what it logs to this server too.">
        <label for="{id}-syslog">Send the log</label>
        <input id="{id}-syslog" name="syslog" type="checkbox" defaultChecked={data?.syslog.enabled} />
      </div>
      <div>
        <label for="{id}-server">Server</label>
        <input
          id="{id}-server"
          name="server"
          class="field font-mono"
          autocapitalize="off"
          spellcheck="false"
          defaultValue={data?.syslog.server}
        />
      </div>
      <div title="0 for the usual, 514.">
        <label for="{id}-port">Port</label>
        <input id="{id}-port" name="port" type="number" class="field w-24" min="0" max="65535" defaultValue={data?.syslog.port} />
      </div>
    </Form>
    <div class="@3xl:col-span-2">
      <Form title="Services" {busy} save={(fields) => services(fields)}>
        {#each data?.services ?? [undefined] as service (service?.name)}
          {@const ours = !!service && OURS.includes(service.name)}
          <div title={ours ? "We speak to the BMC over this: change it in the BMC's own pages." : undefined}>
            <label for="{id}-{service?.name}-on">{service && (SERVICES[service.name] ?? service.name)}</label>
            <span class="flex items-center gap-4">
              <input id="{id}-{service?.name}-on" name="{service?.name}-on" type="checkbox" defaultChecked={service?.enabled} disabled={ours} />
              {#if !service || service.port !== null}
                <input
                  name="{service?.name}-port"
                  type="number"
                  class="field w-24"
                  min="1"
                  max="65535"
                  required
                  aria-label="Port"
                  defaultValue={service?.port}
                  disabled={ours}
                />
              {/if}
            </span>
          </div>
        {/each}
      </Form>
    </div>
  </div>
</section>

<section>
  <h2>FRU</h2>
  <Fru fru={data && data.fru} {save} {busy} />
</section>
