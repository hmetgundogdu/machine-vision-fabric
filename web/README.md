# MVF web — terminal-themed roadmap site

A tiny, static single-page site for the MachineVisionFabric roadmap. Terminal aesthetic, the same
colour palette as the CLI dashboard, and the node/edge motif front and centre.

- **Stack:** [SolidJS](https://solidjs.com) + [Tailwind CSS v4](https://tailwindcss.com) + Vite + TypeScript
- **Content:** hero, roadmap (milestone spine + shipped extras), node/edge showcase, contributors
- Roadmap data lives in [`src/data/roadmap.ts`](src/data/roadmap.ts) — mirrors `docs/roadmap.md`.
- Contributors are static in [`src/data/contributors.ts`](src/data/contributors.ts) — add yourself with a PR.

## Develop

```bash
cd web
npm install
npm run dev        # http://localhost:5173
```

## Build

```bash
npm run build      # → web/dist  (static, deploy anywhere)
npm run preview
```

`base` is `./` so `dist/` can be served from any sub-path (e.g. GitHub Pages).
