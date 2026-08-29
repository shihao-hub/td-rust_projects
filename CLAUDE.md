# AGENTS.md

## 仓库结构说明

本目录（`rust_projects`）采用 **monorepo** 结构管理所有 Rust 项目：

- 每个子目录都是一个独立的项目（例如 `mini-http-server`），彼此互不依赖归属关系，各自维护自己的依赖与配置（`Cargo.toml` / `Cargo.lock`）。
- 整个目录作为单一 Git 仓库进行版本管理，不为单个项目单独建仓。
- 新增项目时直接在目录下用 `cargo new` 创建新的子目录即可，不要在子项目内单独 `git init`。
- 在本目录下工作时，先确认目标项目所在子目录，再在该子目录的上下文中执行构建、测试等操作（cargo 命令均在子项目目录内运行）。

## 工具链说明

- 使用 rustup 管理的 stable-x86_64-pc-windows-msvc 工具链，链接器来自 VS Build Tools 2022。
- rustup/cargo 均已配置 rsproxy.cn 国内镜像（环境变量 `RUSTUP_DIST_SERVER` / `RUSTUP_UPDATE_ROOT` + `~/.cargo/config.toml`）。
