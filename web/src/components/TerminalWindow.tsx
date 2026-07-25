import { JSX } from "solid-js";

/** macOS-style terminal window chrome that wraps the whole page. */
export function TerminalWindow(props: { title: string; children: JSX.Element }) {
  return (
    <div class="mx-auto w-full max-w-5xl overflow-hidden rounded-xl border border-line bg-terminal shadow-2xl shadow-black/40">
      <div class="flex h-9 items-center gap-2 border-b border-line bg-panel px-4">
        <span class="h-3 w-3 rounded-full bg-[#ff5f56]" />
        <span class="h-3 w-3 rounded-full bg-[#ffbd2e]" />
        <span class="h-3 w-3 rounded-full bg-[#27c93f]" />
        <span class="ml-3 truncate text-xs text-dim">{props.title}</span>
      </div>
      <div class="p-5 sm:p-8 lg:p-10">{props.children}</div>
    </div>
  );
}
