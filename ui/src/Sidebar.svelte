<!-- Our devices, the open ones with their power, then those found on the network, which we look for on every start: our
     network and our devices', as a /24 takes seconds. -->
<script lang="ts">
  import { LoaderCircle, Search, Trash2, X } from "@lucide/svelte";
  import { Channel, invoke } from "@tauri-apps/api/core";
  import type { Device as Saved } from "./bindings/ooblin";
  import type { Found } from "./bindings/core";
  import { FLAVORS, reason, type Connection } from "./connection";

  let {
    devices,
    connections,
    current,
    connecting,
    collapsed,
    say,
    onselect,
    ondisconnect,
    onforget,
    onnew,
  }: {
    devices: Saved[];
    connections: Record<string, Connection>;
    current: string | null;
    connecting: Set<string>;
    collapsed: boolean;
    say: (text: string, failed?: boolean) => void;
    onselect: (device: Saved) => void;
    ondisconnect: (host: string) => void;
    onforget: (host: string) => Promise<unknown>;
    onnew: () => void;
  } = $props();

  /** What answered our presence pings by host, the networks we look on and whether we are, see `discover`. */
  let found: Record<string, Found> = $state({});
  let networks = $state("");
  let discovering = $state(false);
  /** Our devices, then those we only found, which we know too little of to log in to without the connect form. */
  let listed: Saved[] = $derived([
    ...devices,
    ...Object.keys(found)
      .filter((host) => !devices.some((device) => device.host === host))
      .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }))
      .map((host) => {
        const user = found[host].flavor === "Amt" ? "admin" : "ADMIN";
        return { host, name: "", version: found[host].version, user, remembered: false, flavor: found[host].flavor };
      }),
  ]);

  async function forget(host: string) {
    await onforget(host);
    delete found[host];
  }

  /** Looks for devices on `networks` until they answered or we stop, says how many did. */
  async function discover() {
    discovering = true;
    let answered = 0;
    const channel = new Channel<Found>((device) => ((found[device.host] = device), answered++));
    try {
      await invoke("discover", { networks, found: channel });
    } catch (error) {
      say(reason(error), true);
    } finally {
      discovering = false;
    }
    return answered;
  }

  invoke<string>("networks").then((nearby) => ((networks = nearby), discover()));
</script>

<nav class={["flex min-w-0 flex-col gap-3 overflow-hidden border-r border-line bg-pane p-3", collapsed && "invisible"]}>
  <p class="px-2.5 pt-1 font-semibold text-muted">Devices</p>
  {#if listed.length}
    <ul class="grid flex-1 content-start gap-1 overflow-y-auto">
      {#each listed as device (device.host)}
        {@const open = connections[device.host]}
        {@const answer = found[device.host]}
        {@const name = open?.info.name || device.name}
        {@const version = device.version || answer?.version}
        {@const kind = open?.info.flavor ?? device.flavor}
        {@const flavor = kind && FLAVORS[kind]}
        <!-- One action per device, on hover: disconnect an open one, forget a closed one. -->
        <li
          class="group flex items-center rounded-md pr-0.5 text-muted hover:bg-hover data-open:text-ink aria-current:bg-accent aria-current:text-white"
          data-open={open || connecting.has(device.host) ? "" : undefined}
          aria-current={device.host === current}
        >
          <button
            type="button"
            class="flex min-w-0 flex-1 items-center gap-2.5 px-2.5 py-1.5 text-left"
            title={answer?.provisioned === false ? "Not provisioned" : undefined}
            onclick={() => onselect(device)}
          >
            {#if connecting.has(device.host)}
              <LoaderCircle class="-mx-0.5 size-3.5 flex-none animate-spin" />
            {:else}
              <span
                class={["dot", !open && "invisible", open?.changing && "animate-pulse"]}
                data-power={open?.power?.state}
                title={open?.power?.state}
              ></span>
            {/if}
            <!-- Its name, and the address we reach it at. -->
            <span class="grid min-w-0">
              <span class="truncate">{name || device.host}</span>
              {#if name}
                <span class="truncate font-mono text-xs text-muted group-aria-current:text-white/80">{device.host}</span>
              {/if}
            </span>
            <!-- What kind of device it is, in its color, and its firmware version. -->
            {#if flavor || version}
              <span
                class={[
                  "ml-auto flex-none rounded-full px-1.5 text-[10px]/4 font-medium tabular-nums group-aria-current:bg-white/20",
                  flavor ? `${flavor.color} text-white` : "bg-hover",
                ]}
                title={[flavor?.title, version && `firmware ${version}`].filter(Boolean).join(", ")}
              >
                {[flavor?.label, version].filter(Boolean).join(" ")}
              </span>
            {/if}
          </button>
          <button
            type="button"
            class="btn-icon hidden size-7 group-hover:grid group-has-[:focus-visible]:grid group-aria-current:text-white/80 group-aria-current:hover:bg-white/20"
            title={open ? "Disconnect" : "Forget"}
            onclick={() => (open ? ondisconnect(device.host) : forget(device.host))}
          >
            {#if open}<X class="size-4" />{:else}<Trash2 class="size-4" />{/if}
          </button>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="grid flex-1 place-items-center text-muted">{discovering ? "Looking for devices…" : "No devices yet"}</p>
  {/if}
  <button type="button" class="btn btn-secondary" onclick={onnew}>New connection</button>
  <!-- Where to look for devices, our own network to start with. -->
  <form
    class="field flex h-8 items-center pr-0 pl-2.5 font-mono text-xs"
    onsubmit={async (event) => {
      event.preventDefault();
      if (discovering) return invoke("stop_discovery");
      const answered = await discover();
      say(
        answered
          ? `${answered} device${answered === 1 ? "" : "s"} answered on ${networks}.`
          : `No device answered on ${networks}.`,
      );
    }}
  >
    <input
      bind:value={networks}
      autocapitalize="off"
      spellcheck="false"
      placeholder="192.168.1.0/24"
      required
      title="Addresses, ranges like 192.168.88.10-192.168.88.50 and networks like 192.168.88.0/22, separated by commas. A /24
takes seconds, a /16 (the most at a time) minutes."
    />
    <button class="btn-icon size-7 flex-none" title={discovering ? "Stop looking" : "Look for devices there"}>
      {#if discovering}<LoaderCircle class="size-4 animate-spin" />{:else}<Search class="size-4" />{/if}
    </button>
  </form>
</nav>
