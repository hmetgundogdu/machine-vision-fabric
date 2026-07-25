import { For } from "solid-js";
import cliDemo from "../assets/cli-demo.svg";

const cats: { name: string; cls: string }[] = [
  { name: "source", cls: "bg-src" },
  { name: "compute", cls: "bg-compute" },
  { name: "classify", cls: "bg-classify" },
  { name: "flow", cls: "bg-flow" },
  { name: "sink", cls: "bg-sink" },
  { name: "value", cls: "bg-value" },
];

/** The live CLI dashboard — the centrepiece, right under the hero. */
export function CliShot() {
  return (
    <div class="rise mt-8">
      <div class="overflow-hidden rounded-lg border border-line bg-panel2">
        <img
          src={cliDemo}
          alt="mvf live graph dashboard running inspection-demo — colour-coded nodes, typed edges, live log panel"
          class="block w-full"
          width="960"
          height="582"
        />
      </div>

      <div class="mt-3 flex flex-wrap items-center gap-x-5 gap-y-2 text-xs text-dim">
        <span class="text-fg/80">the live graph dashboard</span>
        <span>
          <span class="text-src">──</span> data edge
        </span>
        <span>
          <span class="text-classify">╌╌</span> control edge
        </span>
        <span class="flex flex-wrap items-center gap-3">
          <For each={cats}>
            {(c) => (
              <span class="flex items-center gap-1.5">
                <span class={`h-2 w-2 rounded-full ${c.cls}`} />
                {c.name}
              </span>
            )}
          </For>
        </span>
      </div>
    </div>
  );
}
