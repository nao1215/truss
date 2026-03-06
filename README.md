# truss

Rust で実装する image toolkit です。`Truss` はライブラリを中核に据え、CLI、HTTP サーバー、WASM から同じ変換エンジンを利用できる形を目指します。

初期スコープは画像と SVG です。

公開前の開発段階では、後方互換性は保証しません。`doc` 配下の仕様と API surface は、実装完了までは破壊的に変更されることがあります。

現状のコードベースには最小の HTTP サーバー雛形が入っており、`/health`、`/health/live`、`/health/ready` に応答します。  
設計上の目標は `doc` 配下の文書を参照してください。

## Design Docs

- `doc/runtime-architecture.md`: library-first の全体設計
- `doc/cli.md`: CLI コマンドとオプション設計
- `doc/api.md`: HTTP API 仕様と使い勝手の方針
- `doc/desgin.md`: server runtime の実装設計
- `doc/openapi.yaml`: server runtime の OpenAPI source of truth

## Prerequisites

Rust の stable toolchain が必要です。

```sh
rustup toolchain install stable
```

## Build

```sh
cargo build
```

リリースビルド:

```sh
cargo build --release
```

## Run

デフォルトでは `127.0.0.1:8080` で起動します。

```sh
cargo run
```

待ち受けアドレスを変更する場合:

```sh
TRUSS_BIND_ADDR=127.0.0.1:3000 cargo run
```

ヘルスチェック例:

```sh
curl http://127.0.0.1:8080/health
```

## Test

```sh
cargo test
```

CI でも GitHub Actions から同じテストを実行します。
