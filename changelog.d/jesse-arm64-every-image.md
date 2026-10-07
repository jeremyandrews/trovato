Every image published to `ghcr.io/jeremyandrews/trovato` now carries both
`linux/amd64` and `linux/arm64`, and the publish workflow fails rather than
pushing a tag that is missing either. arm64 was built only for release tags,
because it cross-built under QEMU and took over three hours, while every push
to `main` published a nightly under a release shaped version tag such as
`0.104.154`. Those nightly tags held amd64 alone, and nothing in the manifest
step checked which platforms it was about to publish, so the single platform
image went out under the full tag set unnoticed. A consumer that tracks
versions read `0.104.154` as newer than the `0.102.0` release, pulled it on
arm64, and failed with `no match for platform in manifest`. arm64 now builds on
GitHub's native `ubuntu-24.04-arm` runner on every run, nightly and release
alike, with no QEMU anywhere, so there is no longer a reason to skip it. The
manifest job refuses to run unless it has exactly one digest from each
platform, and after pushing it inspects the published version tag and fails
unless both platforms are really there. The workflow also takes a
`concurrency` group per ref, after one `v0.104.0` push started two runs that
raced each other over the `0.104.0`, `0.104` and `latest` tags and paid for the
three hour QEMU build twice.
