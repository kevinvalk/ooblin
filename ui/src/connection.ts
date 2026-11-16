import type { Snippet } from "svelte";
import type { Flavor, Info, Power, PowerState } from "./bindings/core";
import type { CommandError } from "./bindings/ooblin";

/** How we name each kind of device: `label` is the word on its pill and `title` its name in full. Every BMC shares the
 * color of IPMI. */
export const FLAVORS: Record<Flavor, { label: string; title: string; color: string }> = {
  Amt: { label: "AMT", title: "Intel AMT", color: "bg-amt" },
  Supermicro: { label: "SMC", title: "Supermicro BMC", color: "bg-ipmi" },
  Ipmi: { label: "IPMI", title: "IPMI BMC", color: "bg-ipmi" },
  Redfish: { label: "Redfish", title: "Redfish BMC", color: "bg-ipmi" },
};

/** An open device, which its Device keeps and App shows too: in the sidebar, the title and the footer. */
export interface Connection {
  host: string;
  info: Info;
  power?: Power;
  silentSince?: number; // Since when it has not answered, see `readPower` in Device

  /** We set this from a power change until the state moves or a minute has passed, as a reset never shows as another
   * state. */
  changing?: { from: PowerState; until: number };

  footer?: Snippet; // What it says in the footer while it is on screen
}

/** Our commands fail with a `CommandError`, Tauri itself with a string, and a promise may reject with nothing. */
export function reason(error: unknown) {
  return (error as CommandError | null)?.message ?? String(error);
}
