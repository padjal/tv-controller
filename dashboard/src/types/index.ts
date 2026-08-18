// Barrel for the ts-rs generated types in this directory.
//
// Hand-maintained: `cargo test -p shared` writes the individual files but does
// not touch this one. Adding a shared type means adding a line here — a missing
// entry shows up as a TypeScript error at the import site, not silently.
export type { AgentStatus } from "./AgentStatus";
export type { Device } from "./Device";
export type { DeviceState } from "./DeviceState";
export type { PlayCommand } from "./PlayCommand";
export type { PlaybackRequest } from "./PlaybackRequest";
export type { RegisterRequest } from "./RegisterRequest";
export type { SseEvent } from "./SseEvent";
export type { SseKind } from "./SseKind";
export type { StopRequest } from "./StopRequest";
export type { Video } from "./Video";
