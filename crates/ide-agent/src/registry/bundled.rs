//! Official standalone builds for hosts without a package runner.

use super::{Binary, Distribution, Entry};

pub(super) const CODEX: &str = "codex";
pub(super) const QODER: &str = "qoder";

pub(super) fn entries() -> [Entry; 2] {
    [codex(), qoder()]
}

fn codex() -> Entry {
    const VERSION: &str = "0.167.2";
    // https://github.com/openai/codex/releases/tag/rust-v0.167.2
    let builds = [
        (
            "linux-x86_64",
            "x86_64-unknown-linux-musl",
            "3d77be1748d35a4bb0e7f8752cde3b11890c63338e8c599f01eafa4a59e17b3d",
        ),
        (
            "linux-aarch64",
            "aarch64-unknown-linux-musl",
            "56b08c1214e062a5b61684bacb5033c4372a01062a47c87c49a32be83af70695",
        ),
        ("darwin-x86_64", "x86_64-apple-darwin", "d0c2ff50bf7366ab89d8213a498eead05ca01515098b290fb37a9b8dcad3e054"),
        ("darwin-aarch64", "aarch64-apple-darwin", "e791290651bce41938fb160b5de2bd9ff8bcf3401ca3e236c73f485fca4be1bb"),
        (
            "windows-x86_64",
            "x86_64-pc-windows-msvc",
            "20b0f94105f40b617bf209b1679b8674ab6d3d8f303e767ae43cf1860a8705da",
        ),
        (
            "windows-aarch64",
            "aarch64-pc-windows-msvc",
            "61d852fb27769c76344a977cdcc5701bb0172606674e45addf0c12962ad6e8ef",
        ),
    ];
    let binary = builds.into_iter().map(|(platform, target, digest)| {
        let cmd = if platform.starts_with("windows") { "bin/codex.exe" } else { "bin/codex" };
        (
            platform.into(),
            Binary {
                archive: format!("https://github.com/openai/codex/releases/download/rust-v{VERSION}/codex-package-{target}.tar.gz"),
                cmd: cmd.into(),
                args: vec!["app-server".into()],
                env: Default::default(),
                sha256: Some(digest.into()),
            },
        )
    }).collect();
    Entry {
        id: CODEX.into(),
        name: "Codex".into(),
        version: VERSION.into(),
        description: "OpenAI Codex CLI".into(),
        repository: Some("https://github.com/openai/codex".into()),
        website: Some("https://developers.openai.com/codex".into()),
        authors: vec!["OpenAI".into()],
        license: Some("Apache-2.0".into()),
        distribution: Distribution { binary, ..Distribution::default() },
    }
}

fn qoder() -> Entry {
    const VERSION: &str = "1.1.87";
    // https://qoder-ide.oss-accelerate.aliyuncs.com/qodercli/channels/1.1.87/manifest.json
    let builds = [
        (
            "linux-x86_64",
            "linux-x64-baseline.tar.gz",
            "738ee635eb868f5963526779ce023addbd63377b77861060a4c2af53bb83c2db",
        ),
        ("linux-aarch64", "linux-arm64.tar.gz", "bb321552d11ebcbcf67fe85ec8dd56c59459edb586b7ba391febe3cec0979314"),
        ("darwin-x86_64", "darwin-x64.tar.gz", "a90ffc0d95ceb45ffbbeee3ed324d8d6f63f6a508715f7ce2dd58146374b73bc"),
        ("darwin-aarch64", "darwin-arm64.tar.gz", "6bacbb3bfc23d4677d1ed811367d713e2a8fdd408d6785b5edaf8c6055fb34eb"),
        ("windows-x86_64", "windows-x64.zip", "a23933e8f82b1a7238a0d12a5c0a94f96beace6928b33da7322f44344515f9d5"),
        ("windows-aarch64", "windows-arm64.zip", "7d96da487c9f362ac5e6fa31b09be72f2e4203b6681ff58e7b3c1a09e7728210"),
    ];
    let binary = builds
        .into_iter()
        .map(|(platform, file, digest)| {
            let cmd = if platform.starts_with("windows") { "qodercli.exe" } else { "qodercli" };
            (
                platform.into(),
                Binary {
                    archive: format!(
                        "https://qoder-ide.oss-accelerate.aliyuncs.com/qodercli/releases/{VERSION}/qodercli-{file}"
                    ),
                    cmd: cmd.into(),
                    args: vec!["--acp".into()],
                    env: Default::default(),
                    sha256: Some(digest.into()),
                },
            )
        })
        .collect();
    Entry {
        id: QODER.into(),
        name: "Qoder CLI".into(),
        version: VERSION.into(),
        description: "Qoder coding agent".into(),
        repository: None,
        website: Some("https://qoder.com".into()),
        authors: vec!["Qoder AI".into()],
        license: Some("proprietary".into()),
        distribution: Distribution { binary, ..Distribution::default() },
    }
}
