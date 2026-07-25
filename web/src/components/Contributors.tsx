import { For } from "solid-js";
import { contributors, repoUrl } from "../data/contributors";

export function Contributors() {
  return (
    <div class="rise grid gap-5">
      <div class="grid gap-3 sm:grid-cols-2">
        <For each={contributors}>
          {(c) => (
            <a
              href={c.url}
              target="_blank"
              rel="noreferrer"
              class="group flex items-center gap-4 rounded-lg border border-line bg-panel/60 p-4 transition-colors hover:border-src/60"
            >
              <img
                src={c.avatar}
                alt={c.name}
                width="56"
                height="56"
                loading="lazy"
                class="h-14 w-14 shrink-0 rounded-md border border-line bg-panel object-cover"
              />
              <div class="min-w-0">
                <div class="truncate font-semibold text-fg">{c.name}</div>
                <div class="text-sm text-src group-hover:underline">@{c.handle}</div>
                <div class="mt-0.5 text-xs text-dim">{c.role}</div>
              </div>
            </a>
          )}
        </For>

        {/* your name here */}
        <a
          href={`${repoUrl}/blob/main/CONTRIBUTING.md`}
          target="_blank"
          rel="noreferrer"
          class="flex items-center justify-center gap-2 rounded-lg border border-dashed border-line p-4 text-sm text-dim transition-colors hover:border-sink/50 hover:text-sink"
        >
          <span>+</span> your name here — open a PR
        </a>
      </div>
    </div>
  );
}
