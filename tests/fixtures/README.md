# Test Fixtures

- `workers-rs-main.bundle` is a pinned real-world git fixture used by the git CLI e2e tests.
- Source repository: `https://github.com/cloudflare/workers-rs`
- Bundled HEAD commit: `2d022abb219a06e5e53c87793b135e24adc55e45`
- The fixture is a full-history single clone bundled with `git bundle create --all` so tests stay offline and deterministic.

Refresh workflow:

```bash
tmpdir=$(mktemp -d)
git clone --branch main --single-branch https://github.com/cloudflare/workers-rs "$tmpdir/workers-rs"
git -C "$tmpdir/workers-rs" bundle create "$tmpdir/workers-rs-main.bundle" --all
cp "$tmpdir/workers-rs-main.bundle" tests/fixtures/workers-rs-main.bundle
```
