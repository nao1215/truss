# Truss ランタイム設計

## 1. 目的

`Truss` は server 専用プロダクトではなく、同じ画像変換エンジンを以下の 4 形態で利用できるように設計する。

- Rust ライブラリ
- CLI
- HTTP サーバー
- WASM（GitHub Pages 上で動くブラウザ UI を含む）

この文書は、実装時に「どこまでを core に置くか」「どの runtime が何を担当するか」で迷わないための全体設計である。

公開前の開発段階では、後方互換性は保証しない。library API、CLI、HTTP API、WASM interface は、実装完了までは破壊的に変更してよい。

---

## 2. 基本方針

### 2.1 library-first

`Truss` の中核は Rust image toolkit ライブラリである。  
CLI、server、WASM はすべてライブラリを呼び出す薄い adapter として実装する。

優先順位:

1. ライブラリ API が自然であること
2. その API を CLI / server / WASM から再利用できること
3. runtime ごとの制約を adapter 側で吸収すること

### 2.2 image-first

初期スコープは画像と SVG に限定する。

この判断により、以下を最初から除外する。

- 時刻ベースのオプション
- Range Request 前提の出力
- 高コスト変換を前提にした job queue

### 2.3 server-first に戻さない

今後の実装では、server の都合だけで core API を決めてはならない。  
特に以下を core に持ち込まない。

- HTTP request / response 型
- signed URL
- Bearer 認証
- CDN / cache の前提
- path / URL の解決処理
- TCP listener や socket 操作
- ブラウザ DOM / JavaScript 依存

### 2.4 変換機能は core に集約する

画像変換の本体ロジックは `Truss` ライブラリに置く。  
runtime 固有の責務は adapter に分離する。

core に置くもの:

- 変換オプションの型
- バリデーション
- 画像 / SVG の変換パイプライン
- 出力メタデータの表現
- 変換エラー

adapter に置くもの:

- CLI 引数解析
- HTTP routing
- 認証 / 認可
- path / URL / upload の解決
- cache
- ブラウザ UI 連携

---

## 3. ターゲット別の役割

| Target | 役割 | 主な利用者 | 備考 |
| --- | --- | --- | --- |
| Library | 変換機能の中核 | Rust 開発者 | 最重要 deliverable |
| CLI | ローカル変換と検証 | 開発者、運用者 | ライブラリの基準 adapter |
| Server | リモート変換 API 提供 | バックエンド、CDN 配下 | 認証、cache、source resolver を持つ |
| WASM | ブラウザ内変換とデモ | エンドユーザー、ドキュメント閲覧者 | GitHub Pages 配信を想定 |

### 3.1 サポート方針

すべての runtime で同じ機能を無理に揃えない。  
ただし、同じ機能を提供する場合は同じ core API を使う。

初期方針:

- 画像変換は library / CLI / server / WASM の共通機能にする
- SVG は library / CLI / server を主対象とし、WASM は段階導入にする
- server 固有の signed URL と cache は adapter 機能として扱う

---

## 4. レイヤー構成

```text
Rust/JS User
  ↓
CLI / Server / WASM adapter
  ↓
Truss public library API
  ↓
Transform core
  ↓
Image / SVG backend
```

原則:

- adapter は入出力境界を担当する
- core は純粋に「与えられた入力をどう変換するか」だけを担当する
- backend 実装は feature flag で切り替え可能にする

---

## 5. 入出力の責務分離

### 5.1 core は bytes ベースを基本にする

core API は、path や URL を直接受け取ることを前提にしない。  
まず adapter が source を解決し、その結果を core に渡す。

推奨の考え方:

- core の入力: bytes、MIME hint、サイズ制限、変換オプション
- core の出力: bytes、MIME、width / height、付随メタデータ

理由:

- library が server 依存にならない
- WASM でも同じ API を使える
- path / URL / upload の差異を core に持ち込まずに済む

### 5.2 source 解決は adapter の責務

以下は adapter 側で処理する。

- ファイルパスを読んで bytes にする
- URL から取得して bytes にする
- multipart upload を bytes にする
- browser `File` / `Blob` を `Uint8Array` にする

### 5.3 出力の永続化も adapter の責務

以下は core の責務に含めない。

- どこへ保存するか
- HTTP response をどう返すか
- CLI でどのファイル名へ書くか
- browser で download させるか preview するか

---

## 6. crate / module 設計指針

現時点では単一 crate でもよいが、責務は明確に分ける。

推奨構成:

```text
src/
  lib.rs                # public library API
  core/                 # request, validation, pipeline, error
  codecs/               # image, svg backend
  adapters/
    cli.rs              # CLI 共通処理
    server.rs           # server 共通処理
    wasm.rs             # wasm bindgen 用処理
  bin/
    truss-cli.rs
    truss-server.rs
```

実装上の原則:

- `src/lib.rs` から public API を公開する
- `main.rs` に変換ロジックを置かない
- runtime ごとの分岐を `lib.rs` に直書きしない
- feature flag で不要な依存を切れるようにする

想定 feature:

- `image`
- `svg`
- `cli`
- `server`
- `wasm`

---

## 7. public library API の方針

### 7.1 API は adapter 非依存にする

public API は「どこから来た入力か」ではなく、「何をどう変換したいか」を表現する。

推奨の形:

```text
input bytes + image transform options -> transformed artifact
```

### 7.2 変換要求は明示的な型にする

推奨事項:

- `ImageTransformRequest` のような要求型を置く
- 出力は `ImageTransformResult` のような型で返す
- option を runtime ごとの map で持たない
- SVG は image family の一部として扱い、入力種別で分岐する

### 7.3 オプション名を runtime 横断で揃える

同じ意味のオプションに別名を増やさない。  
基準は library のフィールド名とし、他 runtime はそこへ合わせる。

命名規則:

- library / JSON body / query parameter: `lowerCamelCase`
- CLI flag: `kebab-case`

例:

- `autoOrient` -> `--auto-orient`
- `stripMetadata` -> `--strip-metadata`
- `preserveExif` -> `--preserve-exif`

### 7.4 デフォルト値も揃える

runtime ごとにデフォルトを変えない。  
最低限、以下は共通にする。

- `autoOrient = true`
- `stripMetadata = true`
- `fit` は `width` と `height` の両方がある場合のみ意味を持ち、既定値は `contain`
- `position = center`

### 7.5 エラー型は共通化する

最低限、以下を区別できるようにする。

- input 不正
- 未対応フォーマット
- 制限超過
- runtime capability 不足
- backend 実行失敗

---

## 8. runtime ごとの責務

### 8.1 CLI

CLI は最小 adapter とする。  
詳細は [`doc/cli.md`](./cli.md) を参照する。

責務:

- 引数解析
- ファイル読み書き
- 標準出力 / 標準エラー
- exit code 管理

非責務:

- 変換ロジックの独自実装
- server 専用オプションの再定義

### 8.2 Server

server は remote execution adapter である。  
詳細は [`doc/api.md`](./api.md) と [`doc/openapi.yaml`](./openapi.yaml) を参照する。

責務:

- HTTP API
- 認証 / 認可
- source resolver
- cache
- rate limit
- metrics

非責務:

- 画像変換ロジックの独自実装
- CLI や WASM と異なる変換意味論の導入

### 8.3 WASM

WASM は browser execution adapter である。

責務:

- JavaScript から呼びやすい binding
- `Uint8Array` / `Blob` / `File` との相互変換
- UI から必要な最小限の非同期境界

制約:

- ローカル path 入力は扱わない
- secret を必要とする認証機能は持ち込まない
- remote fetch は呼び出し側アプリで制御できるようにする

### 8.4 Library

library は最も安定した契約である。

責務:

- 変換 API の提供
- 互換性の維持
- テストしやすい純粋な API の提供

---

## 9. 機能の適用範囲

実装時に迷わないよう、初期スコープを以下に固定する。

### 9.1 画像系

最優先で共通化する。

- decode
- resize
- fit
- position
- rotate
- auto orient
- format conversion
- metadata 制御

### 9.2 SVG

方針:

- sanitize と rasterize を library の責務として持てる形を目指す
- `format=svg` のときは sanitize 済み SVG を返せるようにする
- browser 制約が強い場合、WASM では機能縮小を許可する

### 9.3 初期スコープ外

以下は初期設計から外す。

- 非同期 job API
- batch pipeline DSL
- core からの直接的な path / URL / storage アクセス
- 高度な矩形 crop API

`crop` は便利に見えるが、座標系、EXIF、SVG、fit との関係で API を複雑にしやすい。  
初期 API は `fit` と `position` に絞り、よく使う操作を単純にする。

---

## 10. 実装順序

実装は以下の順序を推奨する。

1. `src/lib.rs` を変換ライブラリの入口にする
2. `ImageTransformOptions`、request、result、error 型を固める
3. raster image backend を library 呼び出しで成立させる
4. SVG sanitize / rasterize を library に追加する
5. CLI を library 呼び出しで成立させる
6. server を library 呼び出しへ寄せる
7. WASM binding を追加する

この順序により、runtime ごとの差ではなく core API の妥当性を先に検証できる。

---

## 11. LLM 実装ルール

LLM が実装する際は、以下を守ること。

1. 新しい変換機能は、まず library API に追加する
2. 初期スコープ外の抽象化を先回りで入れない
3. path / URL / HTTP request を core の基本入力にしない
4. 新しいオプションを追加したら library、CLI、HTTP docs の命名を揃える
5. `main.rs` や HTTP handler に変換ロジックを直接書かない
6. 初期スコープ外の機能を入れる場合は、先に `doc` を更新して判断理由を残す

判断に迷った場合は、常に「この変更は Rust image toolkit として自然か」を先に評価すること。
