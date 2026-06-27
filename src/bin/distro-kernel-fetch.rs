// The distro kernel fetcher lives in its own repo now (distro-kernel-fetcher,
// the canonical source — also consumed by bcachefs-module-server). This is a
// thin wrapper so ktest still produces a `distro-kernel-fetch` binary, which
// kernel-store-producer invokes as target/release/distro-kernel-fetch.

fn main() -> anyhow::Result<()> {
    distro_kernel_fetcher::run()
}
