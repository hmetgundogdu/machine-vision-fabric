import { For, Show } from "solid-js";
import { milestones, extras, statusMeta, type Milestone } from "../data/roadmap";

function Row(props: { m: Milestone }) {
  const s = () => statusMeta[props.m.status];
  return (
    <div class={`border-l-2 ${s().border} pl-3.5 py-2`}>
      <div class="flex items-baseline gap-2.5 flex-wrap">
        <span class={`rounded px-1.5 py-0.5 text-[11px] font-bold ${s().chip}`}>{props.m.id}</span>
        <span class="font-semibold text-fg">{props.m.title}</span>
        <span class={`ml-auto text-[11px] ${s().text}`}>
          {s().glyph} {s().label}
        </span>
      </div>
      <p class="mt-1 text-sm leading-snug text-dim">{props.m.summary}</p>
      <Show when={props.m.items}>
        <div class="mt-2 flex flex-wrap gap-1.5">
          <For each={props.m.items}>
            {(it) => {
              const is = () => statusMeta[it.status];
              return (
                <span
                  class={`inline-flex items-center gap-1.5 rounded border border-line/70 px-1.5 py-0.5 text-[11px] text-fg/75`}
                >
                  <span class={`h-1.5 w-1.5 rounded-full ${is().dot}`} />
                  {it.label}
                </span>
              );
            }}
          </For>
        </div>
      </Show>
    </div>
  );
}

export function Roadmap() {
  return (
    <div class="rise">
      <div class="grid gap-1">
        <For each={milestones}>{(m) => <Row m={m} />}</For>
      </div>

      <div class="mt-7 mb-2.5 text-xs text-dim">
        <span class="text-value">shipped</span> alongside the spine
      </div>
      <div class="grid gap-2 sm:grid-cols-3">
        <For each={extras}>
          {(m) => (
            <div class="rounded-md border border-ok/40 bg-panel/50 p-3">
              <div class="flex items-center gap-1.5 text-sm font-semibold text-fg">
                <span class="text-ok">✔</span>
                {m.title}
              </div>
              <p class="mt-1.5 text-xs leading-snug text-dim">{m.summary}</p>
            </div>
          )}
        </For>
      </div>
    </div>
  );
}
