import type { Flavor } from "./bindings/core";

/** How each kind of device is named, in a word on its pill and in full, in its color. Both BMCs share IPMI's. */
export const FLAVORS: Record<Flavor, { label: string; title: string; color: string }> = {
  Amt: { label: "AMT", title: "Intel AMT", color: "bg-amt" },
  Supermicro: { label: "SMC", title: "Supermicro BMC", color: "bg-ipmi" },
  Ipmi: { label: "IPMI", title: "IPMI BMC", color: "bg-ipmi" },
};
