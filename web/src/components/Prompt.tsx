import { JSX } from "solid-js";

/** A shell prompt line used to introduce each section. */
export function Prompt(props: { cmd: string; cursor?: boolean }) {
  return (
    <div class="flex items-baseline gap-1.5 text-sm sm:text-base">
      <span class="text-sink">mvf@edge</span>
      <span class="text-dim">:</span>
      <span class="text-src">~</span>
      <span class="text-dim">$</span>
      <span class="text-fg">{props.cmd}</span>
      {props.cursor && <span class="cursor text-fg">▋</span>}
    </div>
  );
}

/** A `# comment` line. */
export function Comment(props: { children: JSX.Element }) {
  return <div class="text-dim text-sm"># {props.children}</div>;
}
