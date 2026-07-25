import { Btn } from "./parts";
import { repoUrl } from "../data/contributors";

export function Hero() {
  return (
    <header class="rise">
      <div class="text-xs text-dim">// open-source · edge-first · Apache-2.0</div>

      <h1 class="mt-3 text-3xl sm:text-5xl font-bold tracking-tight text-fg">
        MachineVision<span class="text-src">Fabric</span>
      </h1>

      <p class="mt-4 max-w-2xl text-base leading-relaxed text-fg/90">
        A high-performance, edge-first <span class="text-value">polyglot pipeline engine</span> — a
        strict, typed graph of <span class="text-compute">.NET</span> /{" "}
        <span class="text-flow">Python</span> / <span class="text-classify">C++</span> nodes that pass
        payloads <span class="text-sink">zero-copy</span> through shared memory. No network in the
        runtime; it runs local-first at the edge.
      </p>

      <div class="mt-6 flex flex-wrap gap-3">
        <Btn href={repoUrl} primary>
          ★ GitHub
        </Btn>
        <Btn href={`${repoUrl}/blob/main/docs/roadmap.md`}>roadmap.md</Btn>
        <Btn href={`${repoUrl}/blob/main/docs/cli-guide.md`}>CLI guide</Btn>
      </div>
    </header>
  );
}
