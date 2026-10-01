# cosh

[English](../../README.md) | [Português](./README.pt-BR.md) | [Español](./README.es.md) | [Русский](./README.ru.md) | [中文](./README.zh-CN.md) | **日本語**

Cosh はターミナル上で動作する汎用コーディングエージェントです。ソフトウェア開発ツールに加え、*computer tools* を備えており、グラフィカルアプリケーションとも対話できます。

<!-- Demo: 下のリンクを、GitHub の README エディタに demox.mp4 をドラッグ＆ドロップした際に生成される https://github.com/user-attachments/assets/... の URL に置き換えてください -->

[cosh デモ](https://github.com/user-attachments/assets/demox-placeholder)

主な特徴:

- **accessibility tree による computer tools** — エージェントは構造化されたコンパクトな表現を通じてグラフィカル UI を読み取り操作し、スクリーンショットはフォールバックとしてのみ使用します。処理する画像が減り、トークン消費も削減されます。
- **Rust でゼロから構築された TUI**（Ratatui）。UX は OpenCode に着想を得ています。プロバイダー、モデル、動作モードの設定はすべてインターフェース上で完結 — 手動設定はほぼ不要です。
- **独立した crates** — 開発ツールはアプリ本体から切り離された crates として提供され、Cosh の外でもライブラリとして再利用できます。

## インストール

Linux と Windows でテスト済み。macOS は未テストです — 問題が発生した場合は、できるだけ詳細を添えて [issue を作成](https://github.com/DiegoZaluski/cosh/issues)してください。

**Linux、macOS、Windows（Git Bash / WSL）:**

```sh
curl -fsSL https://raw.githubusercontent.com/DiegoZaluski/cosh/main/download.sh | bash
```

オプション変数: `COSH_BIN_DIR`（インストール先ディレクトリ）、`COSH_VERSION`（特定バージョン）、`COSH_VARIANT`（デフォルトは `slim`、fastembed によるローカル RAG 付きは `slim-embed`）。

インストール後、ターミナルで `cosh` を実行してください。インストールディレクトリが `PATH` にない場合は、スクリプトが追加用のコマンドを表示します。インストールで問題があれば issue を作成してください — 喜んでお手伝いします！

## Crates

| Crate | 説明 |
|---|---|
| [`cosh-tools`](../../crates/cosh-tools) | 開発ツールと computer tools |
| [`cosh-tui`](../../crates/cosh-tui) | ターミナル UI（Ratatui） |
| [`cosh-sdk`](../../crates/cosh-sdk) | cosh 上にエージェントを構築するための SDK |
| [`cosh-recall`](../../crates/cosh-recall) | メモリとコンテキストの再呼び出し |

## ロードマップ

- **Laya** — harness を補助する小型の意思決定モデル。別バージョンの feature として提供予定。
- **グラフによる harness オーケストレーション** — 複数の harness を協調させる新しいセッションモード。

## ライセンス

Apache-2.0.
