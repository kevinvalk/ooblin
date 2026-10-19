<!-- The window: devices on the left, the connect form or a session on the right, a status line at the bottom. -->
<script lang="ts">
    import { LoaderCircle, PanelLeft, Search, Trash2, X } from "@lucide/svelte";
    import { Channel, invoke } from "@tauri-apps/api/core";
    import type { CommandError, Device } from "./bindings/console";
    import type { Found, Info, Power } from "./bindings/core";
    import Connect from "./Connect.svelte";
    import Session, { type Session as Open } from "./Session.svelte";

    /** Every device we logged in to, the open ones by host, and the one on screen (none: the connect form). */
    let devices: Device[] = $state([]);
    let sessions: Record<string, Open> = $state({});
    let current: string | null = $state(null);
    /** What answered our presence pings by host, the networks we look on and whether we are, see `discover`. */
    let found: Record<string, Found> = $state({});
    let networks = $state("");
    let discovering = $state(false);
    /** Our devices, then those we only found, which we know too little of to log in to without the connect form. */
    let listed: Device[] = $derived([
        ...devices,
        ...Object.keys(found)
            .filter((host) => !devices.some((device) => device.host === host))
            .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }))
            .map((host) => ({
                host,
                name: "",
                version: found[host].version,
                user: "admin",
                remembered: false,
            })),
    ]);
    /** The connect form, for `device` if given: a new `key` is a fresh one. `error` is why the last attempt failed. */
    let form = $state({
        key: 0,
        device: undefined as Device | undefined,
        error: undefined as CommandError | undefined,
    });
    /** The device we're connecting to: it spins, and a second click on it does nothing. */
    let connecting: string | null = $state(null);
    let collapsed = $state(false);
    /** The disk image in the drive of the device on screen, see Session. */
    let image = $derived(current ? sessions[current]?.image : undefined);
    /** How fast AMT reads a disk image at most, bytes a second. */
    const TOP = 1.1e6;
    /** And its screen, while shown, see Kvm. */
    let screen = $derived(current ? sessions[current]?.screen : undefined);
    let message = $state({ text: "", failed: false });
    /** What the screen or serial console of the device on screen waits for, under what was just said. */
    let waiting = $derived(Object.values((current && sessions[current]?.status) || {}).find(Boolean));
    let hide = 0;

    /** Says `text` in the footer for as long as it takes to read: five seconds, more for more words, fifteen at most.
     * What `failed` stands out. */
    function say(text: string, failed = false) {
        message = { text, failed };
        clearTimeout(hide);
        hide = setTimeout(() => (message.text = ""), Math.min(5000 + 50 * text.length, 15_000));
    }

    async function listDevices() {
        devices = await invoke<Device[]>("devices").catch((error) => (say(String(error), true), []));
    }

    /** Open sessions show, remembered devices log in, the rest ask for their password. */
    function select(device: Device) {
        if (sessions[device.host]) return show(device.host);
        if (connecting === device.host) return;
        showForm(device);
        if (device.remembered)
            connect({
                host: device.host,
                user: device.user,
                password: null,
                remember: false,
            });
    }

    function showForm(device?: Device, error?: CommandError) {
        form = { key: form.key + 1, device, error };
        current = null;
        say("");
        listDevices();
    }

    /** Without a `password` with the one we have, see the `connect` command. */
    async function connect(login: { host: string; user: string; password: string | null; remember: boolean }) {
        const host = login.host;
        connecting = host;
        form.error = undefined;
        try {
            sessions[host] = {
                host,
                info: await invoke<Info>("connect", login),
                pages: {},
            };
            show(host);
        } catch (error) {
            form.error = explain(error, `Couldn't reach ${host}`);
        } finally {
            connecting = null;
        }
    }

    /** Our commands fail with a `CommandError`, Tauri itself with a string. A device that didn't answer gets named. */
    function explain(error: unknown, unreachable: string): CommandError {
        const failure =
            typeof error === "string" ? { kind: "Failed" as const, message: error } : (error as CommandError);
        return failure.kind === "Unreachable" ? { ...failure, message: `${unreachable}: ${failure.message}` } : failure;
    }

    function show(host: string) {
        current = host;
        say("");
        refreshPower(host);
        listDevices();
    }

    async function disconnect(host: string) {
        await invoke("disconnect", { host });
        delete sessions[host];
        if (host === current) showForm();
    }

    async function forget(host: string) {
        await invoke("forget", { host });
        delete found[host];
        listDevices();
    }

    /** Looks for devices on `networks` until they answered or we stop, says how many did. */
    async function discover() {
        discovering = true;
        let answered = 0;
        const channel = new Channel<Found>((device) => ((found[device.host] = device), answered++));
        try {
            await invoke("discover", { networks, found: channel });
        } catch (error) {
            say(explain(error, "Couldn't look for devices").message, true);
        } finally {
            discovering = false;
        }
        return answered;
    }

    /** The device stopped answering for good: we let go of it and say why, Connect tries again. */
    function lost(host: string, error: CommandError) {
        invoke("disconnect", { host });
        delete sessions[host];
        const failure = explain(error, `Lost the connection to ${host}`);
        if (host === current)
            showForm(
                devices.find((device) => device.host === host),
                failure,
            );
        else say(failure.message, true);
    }

    /** The hosts whose power we're asking for, one ask at a time: one that doesn't answer takes a while to say so. */
    const asking = new Set<string>();

    /** A device may not answer for a while (AMT can be busy while it resets), only after two minutes is it gone. Until
     * then its power is unknown. */
    async function refreshPower(host: string) {
        const session = sessions[host];
        if (asking.has(host)) return;
        asking.add(host);
        try {
            session.power = await invoke<Power>("power", { host });
            session.silent = undefined;
            const changing = session.changing;
            if (changing && (session.power.state !== changing.from || Date.now() > changing.until)) {
                session.changing = undefined;
            }
        } catch (error) {
            if ((error as CommandError).kind !== "Unreachable") {
                if (host === current) say(explain(error, host).message, true);
            } else if (Date.now() - (session.silent ??= Date.now()) > 120_000) {
                lost(host, error as CommandError);
            } else {
                session.power = undefined;
            }
        } finally {
            asking.delete(host);
        }
    }

    // The window is titled after the session on screen.
    $effect(() => {
        invoke("title", {
            title: current ? `${sessions[current].info.name} – AMT Console` : "AMT Console",
        });
    });

    // Power changes from elsewhere (the OS shutting down, another console) show up within a few seconds.
    setInterval(() => Object.keys(sessions).forEach(refreshPower), 15_000);

    function keydown(event: KeyboardEvent) {
        if ((event.metaKey || event.ctrlKey) && event.key === "b") {
            event.preventDefault();
            collapsed = !collapsed;
        }
    }

    // Started with a host on the command line? Open it as if it was picked from the sidebar.
    invoke<Device | null>("launch").then((device) => (device ? select(device) : showForm()));

    // A /24 takes seconds, so we look around on every start: our network and our devices'.
    invoke<string>("networks").then((nearby) => ((networks = nearby), discover()));
</script>

<svelte:window onkeydown={keydown} />

<div class={["grid h-screen grid-rows-[1fr_auto]", collapsed ? "grid-cols-[0_1fr]" : "grid-cols-[16rem_1fr]"]}>
    <nav
        class="flex min-w-0 flex-col gap-3 overflow-hidden border-r border-line bg-pane p-3"
        class:invisible={collapsed}
    >
        <p class="px-2.5 pt-1 font-semibold text-muted">Devices</p>
        {#if listed.length}
            <ul class="grid flex-1 content-start gap-1 overflow-y-auto">
                {#each listed as device (device.host)}
                    {@const open = sessions[device.host]}
                    {@const answer = found[device.host]}
                    <!-- One action per device, on hover: disconnect an open one, forget a closed one. -->
                    <li
                        class="group flex items-center rounded-md pr-0.5 text-muted hover:bg-hover data-open:text-ink aria-current:bg-accent aria-current:text-white"
                        data-open={open || connecting === device.host ? "" : undefined}
                        aria-current={device.host === current}
                    >
                        <button
                            type="button"
                            class="flex h-8 min-w-0 flex-1 items-center gap-2.5 px-2.5"
                            title={[device.name && device.host, answer?.provisioned === false && "not provisioned"]
                                .filter(Boolean)
                                .join(", ")}
                            onclick={() => select(device)}
                        >
                            {#if connecting === device.host}
                                <LoaderCircle class="-mx-0.5 size-3.5 flex-none animate-spin" />
                            {:else}
                                <span
                                    class="dot"
                                    class:invisible={!open}
                                    class:animate-pulse={open?.changing}
                                    data-power={open?.power?.state}
                                    title={open?.power?.state}
                                ></span>
                            {/if}
                            <span class="truncate">{device.name || device.host}</span>
                            {#if device.version || answer}
                                <span
                                    class="flex-none rounded-full bg-hover px-1.5 text-xs/4.5 tabular-nums group-aria-current:bg-white/20"
                                    title="AMT version"
                                >
                                    {device.version || answer?.version}
                                </span>
                            {/if}
                        </button>
                        <button
                            type="button"
                            class="btn-icon hidden size-7 group-hover:grid group-aria-current:text-white/80 group-aria-current:hover:bg-white/20"
                            title={open ? "Disconnect" : "Forget"}
                            onclick={() => (open ? disconnect(device.host) : forget(device.host))}
                        >
                            {#if open}<X class="size-4" />{:else}<Trash2 class="size-4" />{/if}
                        </button>
                    </li>
                {/each}
            </ul>
        {:else}
            <p class="grid flex-1 place-items-center text-muted">
                {discovering ? "Looking for devices…" : "No devices yet"}
            </p>
        {/if}
        <button type="button" class="btn btn-secondary" onclick={() => showForm()}>New connection</button>
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

    <div class="grid min-w-0 overflow-hidden">
        {#if current}
            <Session bind:session={sessions[current]} {say} />
        {:else}
            {#key form.key}
                <Connect
                    device={form.device}
                    connecting={connecting !== null}
                    error={form.error}
                    onsubmit={(login) => connect({ ...login, password: login.password || null })}
                    ontrust={(host, fingerprint) => invoke("trust", { host, fingerprint })}
                />
            {/key}
        {/if}
    </div>

    <footer class="col-span-full flex h-8 items-center gap-2 border-t border-line bg-pane px-1.5">
        <button
            type="button"
            class="btn-icon size-7"
            title="Toggle the sidebar (⌘B)"
            onclick={() => (collapsed = !collapsed)}
        >
            <PanelLeft class="size-4" />
        </button>
        <!-- What's true now outlasts what just happened. -->
        {#if current && sessions[current].silent}
            <output class="truncate text-muted">{current} isn't answering, trying again…</output>
        {:else if !message.text && waiting}
            <output class="flex min-w-0 items-center gap-1.5 text-muted">
                <LoaderCircle class="size-3.5 flex-none animate-spin" /><span class="truncate">{waiting}</span>
            </output>
        {:else}
            <!-- Selectable, and kept while the pointer is on it, to copy. -->
            <output
                class={["truncate select-text", message.failed ? "text-danger" : "text-muted"]}
                onmouseenter={() => clearTimeout(hide)}
                onmouseleave={() => message.text && say(message.text, message.failed)}
            >
                {message.text}
            </output>
        {/if}
        <div class="ml-auto flex items-center gap-6 pr-1.5 whitespace-nowrap text-muted tabular-nums">
            <!-- While the screen is shown. -->
            {#if screen}
                <span title="The device's screen, and the updates a second it sends">
                    KVM {screen.width} × {screen.height}, {screen.fps} fps
                </span>
            {/if}
            <!-- The disk image as the device reads it, like a defragmenter's map: its stretches fade for as long as
                 reading all of it at the top speed would take, so a whole read leaves a trail across them. -->
            {#if image}
                <div
                    class="flex gap-0.75"
                    style:--speed={Math.min(image.rate / TOP, 1)}
                    style:--decay="{image.size / TOP}s"
                    title="Disk read {(image.rate / 1e6).toFixed(2)} MB/s which top out at about {TOP /
                        1e6} MB/s as each read goes through a small management engine."
                >
                    <div class="disk-speed"></div>
                    <div class="disk-map">
                        {#each image.cells as read}
                            <!-- Each read restarts its animation, which shown again (another device was) goes on
                                 where it is by now. -->
                            {#key read}<div
                                    data-read={read || undefined}
                                    style:animation-delay="{read - Date.now()}ms"
                                ></div>{/key}
                        {/each}
                    </div>
                </div>
            {/if}
        </div>
    </footer>
</div>
