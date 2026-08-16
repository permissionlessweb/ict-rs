# ict-rs prebuilt-commit E2E

Same design as terp-core `docs/workflows/prebuilt-e2e.md`.

Layout: `https://minio.terp.network/releases/ict-rs/commits/<full-sha>/ict-ci-linux-x86_64.tar.gz`

| Recipe / script | Role |
|-----------------|------|
| `just ci-build-docker` | linux/amd64 `ict-ci` + suites (not host uname). |
| `just ci-publish-prebuilt` | Upload tarball for this git SHA. |
| `just ci-fetch-prebuilt` | Download + sha256. No compile. |
| `just ci-fetch-terp` | Load pinned terp-core image (`scripts/ci/terp-core.env`). |
| `Dockerfile.ci` | Rebuild path for identity check. Context = parent `crates/`. |
| `.github/workflows/e2e-prebuilt.yml` | `e2e-tests` \\|\\| `rebuild` → `assert-identical`. Dispatch only. |

Publish under the SHA that produced the bits. ELF must be x86-64.
