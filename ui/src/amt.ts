// The commands in crates/amt-console/src/main.rs.

import { invoke } from "@tauri-apps/api/core";

export interface Target {
  host: string;
  user: string;
  password: string;
}

/** A device we logged in to before. `remembered` means we can log in again without asking for the password. */
export interface Device {
  host: string;
  user: string;
  remembered: boolean;
}

export interface Info {
  hostname: string;
  domain: string;
  version: string;
  build: string | null;
  controlMode: "admin" | "client" | "unknown";
  digestRealm: string;
  fingerprint: string;
  trust: "pinned" | "firstUse";
}

export type PowerState = "on" | "sleep" | "hibernate" | "off" | "unknown";
export type PowerAction = "on" | "off" | "cycle" | "reset";

export interface Inventory {
  manufacturer: string;
  model: string;
  serial: string;
  board: string;
  bios: string;
  biosDate: string;
  processor: string;
  memory: { bank: string; bytes: number; mhz: number; part: string }[];
}

/** Our commands fail with one of these, Tauri itself with a plain string. */
export type CommandError =
  | { kind: "pinMismatch"; message: string; expected: string; actual: string }
  | { kind: "failed"; message: string };

export const launch = () => invoke<Device | null>("launch");
export const devices = () => invoke<Device[]>("devices");
export const connect = (target: Target, remember: boolean) => invoke<Info>("connect", { ...target, remember });
export const reconnect = (host: string) => invoke<Info>("reconnect", { host });
export const disconnect = (host: string) => invoke<void>("disconnect", { host });
export const forget = (host: string) => invoke<void>("forget", { host });
export const trust = (host: string, fingerprint: string) => invoke<void>("trust", { host, fingerprint });
export const powerState = (host: string) => invoke<PowerState>("power_state", { host });
export const power = (host: string, action: PowerAction) => invoke<void>("power", { host, action });
export const inventory = (host: string) => invoke<Inventory>("inventory", { host });
export const title = (title: string) => invoke<void>("title", { title });
