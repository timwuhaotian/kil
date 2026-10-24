# Kil — notes for agents

- `DESIGN.md` is the spec; `CONTRIBUTING.md` has the checks and conventions.
- Work and commit directly on `main`; don't create branches.
- **To release** ("发版", "new patch version", "打包"), follow [`RELEASING.md`](RELEASING.md)
  step by step: bump, changelog, README, `scripts/release.sh`, install into
  `/Applications`, commit `Kil x.y.z: …`, push.
- **Config questions** ("where is the config", "what can I set", "rebind a key"): run the
  `/kil-config` skill (`.claude/skills/kil-config/SKILL.md`). It reads `settings.rs`,
  `keymap.rs` and the user's files instead of answering from memory.
