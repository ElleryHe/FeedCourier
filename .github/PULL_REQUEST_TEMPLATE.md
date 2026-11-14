## Summary

<!-- Briefly describe what this PR changes and why. -->

## Linked Issue

<!-- e.g. Fixes #123, or "N/A" for small fixes. -->

## Verification Checklist

- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test --all-targets` passes (with regression tests added/updated where applicable)
- [ ] `pnpm --dir web exec tsc --noEmit && pnpm --dir web build` passes (if `web/` changed)
- [ ] `cargo deny check licenses bans sources` passes (if dependencies changed)
- [ ] Outbound HTTP SSRF guardrails (`src/net/ssrf.rs`) and `/mcp` credential/recipient trust boundaries are preserved
