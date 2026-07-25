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
    id: "M1",
    title: "Out-of-process module host · Python",
    status: "done",
    summary: "Modules run out-of-process over local stdio (JSON); a Python node auto-wires from its manifest runtime.",
  },
  {
    id: "M2",
    title: "Shared-memory zero-copy data plane",
    status: "done",
    summary: "Our graph-aware arena: typed payloads read/written in place — zero-copy, no base64.",
  },
  {
    id: "M2.5",
    title: "Snapshot + module-state recovery",
    status: "done",
    summary: "Checkpoint/restore state; a crashed worker is restarted, restored and retried; sources resume too.",
  },
  {
    id: "M3",
    title: "Hardening",
    status: "progress",
    summary: "Backpressure · cross-process observability · source-failure honesty. What's left of M3 is the L-track.",
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
    title: "Realtime egress · async publish",
    status: "planned",
    summary: "Async frame-data + frame-state publish to a studio/observer, off the hot path. Concept only — not yet designed.",
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
