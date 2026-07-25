import { JSX } from "solid-js";
import { TerminalWindow } from "./components/TerminalWindow";
import { Prompt } from "./components/Prompt";
import { Hero } from "./components/Hero";
import { CliShot } from "./components/CliShot";
import { Roadmap } from "./components/Roadmap";
import { Contributors } from "./components/Contributors";
import { repoUrl } from "./data/contributors";

function Section(props: { cmd: string; children: JSX.Element }) {
  return (
    <section class="mt-10 border-t border-line2 pt-6">
      <Prompt cmd={props.cmd} />
      <div class="mt-5">{props.children}</div>
    </section>
  );
}

export function App() {
  return (
    <div class="min-h-screen px-3 py-6 sm:px-6 sm:py-10">
      <TerminalWindow title="mvf — roadmap · machine-vision-fabric">
        <Hero />
        <CliShot />

        <Section cmd="mvf roadmap">
          <Roadmap />
        </Section>

        <Section cmd="git shortlog -sne">
          <Contributors />
        </Section>

        <footer class="mt-10 border-t border-line2 pt-5 text-xs text-dim">
          <div class="flex flex-wrap items-center gap-x-4 gap-y-2">
            <a class="hover:text-src" href={repoUrl} target="_blank" rel="noreferrer">
              github.com/hmetgundogdu/machine-vision-fabric
            </a>
            <span class="text-line">·</span>
            <span>Apache-2.0</span>
            <span class="ml-auto text-dim/70">built with SolidJS + Tailwind · no trackers</span>
          </div>
        </footer>
      </TerminalWindow>
    </div>
  );
}
