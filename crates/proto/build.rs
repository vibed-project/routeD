// SPDX-License-Identifier: Apache-2.0
//! Build script: compiles `proto/snapshot.proto`. No system `protoc` in the
//! toolchain container, so this parses with the pure-Rust `protox` and hands
//! tonic-prost-build a `FileDescriptorSet` directly.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fds = protox::compile(["proto/snapshot.proto"], ["proto"])?;
    tonic_prost_build::compile_fds(fds)?;
    Ok(())
}
