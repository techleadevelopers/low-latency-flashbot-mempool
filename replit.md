# Corporate Residual Sweeper (CRS)

A Rust MEV / residual sweeper bot (binary `mev-sweeper`) plus a cyberpunk WebGL HUD frontend.

## Stack

- **Backend (Rust)** — `Cargo.toml` builds `mev-sweeper` plus the toolkit binaries
  in `src/bin/` (`sweeper`, `preapprove`, `predelegate`). The main binary serves
  an axum dashboard at `DASHBOARD_ADDR` (defaults to `127.0.0.1:8787`) and
  exposes `GET /api/status` with the `DashboardState` JSON shape.
- **Frontend (`web/`)** — standalone static dashboard served on port 5000:
  - `index.html` — HUD shell with left sidebar nav and 5 hash-routed views:
    `#dashboard` (stats + Latency Radar + Ranking),
    `#wallets` (operational wallet table with 7702/APR badges),
    `#rpc` (per-endpoint RPC fleet cards with health bar/latency/errors/last block),
    `#eip7702` (per-wallet delegation + pre-approval table with re-apply actions),
    `#events` (full event console)
  - `styles.css` — cyberpunk neon theme (cyan/magenta, scanlines, glitch),
    sidebar + view layout, ops toolbar/table/cards, neon buttons & pills
  - `js/fx.js` — PixiJS WebGL background (hex grid, particles, scan beam, neon arcs)
  - `js/radar.js` — Canvas2D latency radar with central numeric AVG ms readout
  - `js/data.js` — probes `/api/status`; falls back to a realistic simulation
    matching `DashboardState`, plus simulated `delegated_7702`/`preapproved`
    flags + `delegation_summary` until the backend exposes them
  - `js/app.js` — hash router + per-view renderers and main render loop

## Workflow

`Start application` runs:

```
python3 -m http.server 5000 --bind 0.0.0.0 --directory web
```

The Rust backend is **not** started by the workflow — it requires real chain
credentials (RPC keys, wallet keys, contract addresses) per
`.env.paper-hard.example` / `.env.shadow-hard.example`. When the user runs
`mev-sweeper` separately, the frontend will detect `/api/status` and switch
from `SIM` to `LIVE` automatically.

## Building / running the Rust backend

```bash
cargo build --release
cp .env.paper-hard.example .env  # then fill in real values
cargo run --release
```

The frontend will pick up the live dashboard if `DASHBOARD_ADDR` is reachable
on the same origin (proxy `/api/status` → `http://127.0.0.1:8787/api/status`
to combine them in production).

## Deployment

Static frontend deployed via Replit autoscale, serving `web/` over port 5000.
