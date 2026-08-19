export type Status = "done" | "progress" | "gated" | "planned";

export interface SubItem {
  label: string;
  status: Status;
}

export interface Milestone {
  id: string;
  title: string;
  status: Status;
  summary: string;
  items?: SubItem[];
}

/** Static, human-readable class strings so the Tailwind scanner keeps them. */
export const statusMeta: Record<
  Status,
  { label: string; glyph: string; text: string; border: string; chip: string; dot: string }
> = {
  done: {
    label: "done",
    glyph: "✔",
    text: "text-ok",
    border: "border-ok/50",
    chip: "bg-ok/15 text-ok",
    dot: "bg-ok",
  },
  progress: {
    label: "in progress",
    glyph: "▶",
    text: "text-warn",
    border: "border-warn/50",
    chip: "bg-warn/15 text-warn",
    dot: "bg-warn",
  },
  gated: {
    label: "design-gated",
    glyph: "⛔",
    text: "text-gated",
    border: "border-gated/50",
    chip: "bg-gated/15 text-gated",
    dot: "bg-gated",
  },
  planned: {
    label: "planned",
    glyph: "○",
    text: "text-dim",
    border: "border-line",
    chip: "bg-white/5 text-dim",
    dot: "bg-dim",
  },
};

/** The milestone spine (docs/roadmap.md) — one line each. */
export const milestones: Milestone[] = [
  {
    id: "M3",
    title: "Hardening",
    status: "done",
    summary: "Backpressure · cross-process observability · source-failure honesty. Engine hardening complete.",
    items: [
      { label: "backpressure (Stall / Drop)", status: "done" },
      { label: "cross-process observability", status: "done" },
      { label: "source-failure honesty", status: "done" },
    ],
  },
  {
    id: "L",
    title: "Module lifecycle · readiness contract",
    status: "progress",
    summary: "Standards-aligned readiness (K8s probes / OSGi / sd_notify / Triton).",
    items: [
      { label: "L.1 contract observed", status: "done" },
      { label: "L.2 readiness signal", status: "done" },
      { label: "L.3 lazy activate", status: "done" },
      { label: "L.4 warm pools", status: "done" },
      { label: "L.3b idle-unload", status: "gated" },
      { label: "L.5 hot-reload", status: "gated" },
    ],
  },
  {
    id: "EG",
    title: "Realtime egress · live publish",
    status: "done",
    summary:
      "Publish a running pipeline's frame-state + frame-data live to a studio/observer, off the hot path. One framed binary stream; the arena's own descriptor makes frame-data near-zero-copy.",
    items: [
      { label: "TCP / WebSocket / UDP", status: "done" },
      { label: "alive-beacon discovery", status: "done" },
      { label: "TS + .NET consumer SDKs", status: "done" },
      { label: "TUI egress view", status: "done" },
    ],
  },
  {
    id: "M4",
    title: "Frontiers · WASM tier, GPU handles",
    status: "planned",
    summary: "WASM module tier; GPU-resident frames via DLPack / CUDA-IPC. Distributed is a non-goal.",
  },
];

/** Shipped alongside the spine — cross-cutting engine work already in. */
export const extras: Milestone[] = [
  {
    id: "exec",
    title: "Pipelined executor",
    status: "done",
    summary: "Opt-in --mode pipelined: stage + per-node parallelism, joins, epoch-barrier checkpoints. 2.36× over serial.",
  },
  {
    id: "value",
    title: "value / select primitives",
    status: "done",
    summary: "Values a graph can't compute (threshold, folder, camera) + collection narrowing; live-tunable from the TUI.",
  },
  {
    id: "loop",
    title: "loop primitive",
    status: "done",
    summary: "Iteration authority: until-exhausted / forever / count + whole-graph pause. forever rewinds a finite source.",
  },
];
