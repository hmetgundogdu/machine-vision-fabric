import { JSX } from "solid-js";

export type Cat = "src" | "compute" | "classify" | "flow" | "sink" | "value";

// Static class strings so Tailwind keeps them.
const catClass: Record<Cat, string> = {
  src: "border-src/70 text-src",
  compute: "border-compute/70 text-compute",
  classify: "border-classify/70 text-classify",
  flow: "border-flow/70 text-flow",
  sink: "border-sink/70 text-sink",
  value: "border-value/70 text-value",
};

/** A pipeline node, drawn like a box in the CLI graph view. */
export function NodeChip(props: { label: string; cat: Cat; glyph?: string }) {
  return (
    <span
      class={`inline-flex items-center gap-1.5 rounded-md border bg-panel px-2.5 py-1 text-sm ${catClass[props.cat]}`}
    >
      {props.glyph && <span class="opacity-80">{props.glyph}</span>}
      {props.label}
    </span>
  );
}

/** A typed edge between two nodes — solid = data, dashed = control. */
export function Edge(props: { label: string; kind: "data" | "control" }) {
  const color = props.kind === "data" ? "border-src text-src" : "border-classify text-classify";
  return (
    <div class="flex flex-1 items-center gap-2 min-w-[80px]">
      <div
        class={`h-0 flex-1 border-t ${props.kind === "control" ? "border-dashed" : ""} ${color.split(" ")[0]}`}
      />
      <span class={`shrink-0 text-xs ${color.split(" ")[1]}`}>{props.label}</span>
      <div
        class={`h-0 flex-1 border-t ${props.kind === "control" ? "border-dashed" : ""} ${color.split(" ")[0]}`}
      />
      <span class={`shrink-0 ${color.split(" ")[1]}`}>▶</span>
    </div>
  );
}

/** A ghost-button style link. */
export function Btn(props: { href: string; children: JSX.Element; primary?: boolean }) {
  return (
    <a
      href={props.href}
      target={props.href.startsWith("http") ? "_blank" : undefined}
      rel="noreferrer"
      class={
        "inline-flex items-center gap-2 rounded-md border px-3.5 py-2 text-sm transition-colors " +
        (props.primary
          ? "border-sink/60 bg-sink/10 text-sink hover:bg-sink/20"
          : "border-line text-fg hover:border-src/60 hover:text-src")
      }
    >
      {props.children}
    </a>
  );
}
