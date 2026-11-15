import type { Snippet } from "svelte";
import type { Flavor, Info, Power, PowerState } from "./bindings/core";
import type { CommandError } from "./bindings/ooblin";

/** How each kind of device is named, in a word on its pill and in full, in its color. Every BMC shares IPMI's. */
export const FLAVORS: Record<Flavor, { label: string; title: string; color: string }> = {
  Amt: { label: "AMT", title: "Intel AMT", color: "bg-amt" },
  Supermicro: { label: "SMC", title: "Supermicro BMC", color: "bg-ipmi" },
  Ipmi: { label: "IPMI", title: "IPMI BMC", color: "bg-ipmi" },
  Redfish: { label: "Redfish", title: "Redfish BMC", color: "bg-ipmi" },
};

/** An open device, as its Device keeps it and App shows it too: in the sidebar, the title and the footer. */
export interface Connection {
  host: string;
  info: Info;
  power?: Power;
  /** Since when it hasn't answered, see Device's `readPower`. */
  silent?: number;
  /** Set from a power change until the state moves or a minute has passed, a reset never shows as another state. */
  changing?: { from: PowerState; until: number };
  /** What it says in the footer while it's on screen. */
  footer?: Snippet;
}

/** Our commands fail with a `CommandError`, Tauri itself with a string, and a promise may reject with nothing. */
export function reason(error: unknown) {
  return (error as CommandError | null)?.message ?? String(error);
}
