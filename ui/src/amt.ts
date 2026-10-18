// The commands in crates/amt-console/src/main.rs.

import { invoke } from "@tauri-apps/api/core";

export interface Target {
  host: string;
  user: string;
  password: string;
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

export type CommandError =
  | { kind: "pinMismatch"; message: string; expected: string; actual: string }
  | { kind: "failed"; message: string };

export const connect = (target: Target) => invoke<Info>("connect", { target });

export const trust = (host: string, fingerprint: string) => invoke<void>("trust", { host, fingerprint });
