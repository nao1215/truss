# Truss Image Server 設計書

## 1. 前提

本書は `Truss` の server runtime に対する設計書である。  
`Truss` 全体の方針は [`doc/runtime-architecture.md`](./runtime-architecture.md) を優先し、本書はそのうち HTTP サーバーとして公開する機能だけを扱う。  
外部公開 API の source of truth は [`doc/openapi.yaml`](./openapi.yaml) v0.8.0 とし、本書はその実装設計を説明する。  
本書と OpenAPI に差分がある場合は、OpenAPI を優先する。

初期スコープは画像と SVG に限定する。

---

## 2. 設計方針

Truss Server は、画像と SVG のオンデマンド変換を安全かつキャッシュ効率よく提供する。

設計目標:

- server runtime を image-first に絞る
- public GET と private POST の役割を明確に分ける
- path 入力と url 入力を仕様上も明確に分離する
- option 名を CLI / library と揃える
- 同一変換の重複実行を抑止する
- SSRF、path traversal、MIME spoofing、SVG active content を仕様レベルで封じる

---

## 3. API 構成

画像変換:

- `GET /images/by-path`
- `GET /images/by-url`
- `POST /images:transform`
- `POST /images`

運用 API:

- `GET /health`
- `GET /health/live`
- `GET /health/ready`
- `GET /metrics`

---

## 4. なぜこの構成にしたか

### 4.1 `GET /media` をやめた理由

1 つの endpoint に source 選択、変換内容、公開 / 非公開の利用形態まで混ぜると、長期運用で破綻しやすい。

この構造だと以下の問題が出る。

- SDK 実装が複雑になる
- CDN キャッシュキーが爆発する
- OpenAPI 上でパラメータ制約を表現しづらい
- private 利用でも query string に依存しやすくなる

そのため、初期仕様では入口を用途ごとに分ける。

### 4.2 `by-path` / `by-url` に分けた理由

`path` と `url` を同一 endpoint に置くと、排他制約を OpenAPI で表現しづらい。  
そのため、source kind ごとに endpoint を分離し、仕様上も 1 つの source だけを取る形にする。

### 4.3 `POST /images:transform` を入れる理由

private API が upload 専用だと、server-to-server 利用でも毎回 multipart を組み立てる必要がある。  
これは実装もテストも不便で、署名付き GET と役割が重なりやすい。

そのため、private API には JSON で扱える `POST /images:transform` を用意する。

### 4.4 `crop` と `orientation` を外した理由

`crop` と EXIF `orientation` 番号は、API 利用者にとって分かりにくい。

- `crop` は座標系と評価順の説明が重い
- `orientation` は EXIF 固有知識を要求する

そのため、初期仕様では以下に絞る。

- `fit`
- `position`
- `rotate`
- `autoOrient`

### 4.5 非同期 job API を外した理由

画像変換だけを初期対象にするなら、まずは同期 API の使い勝手を磨くべきである。  
job queue を先に入れると、実装責務と運用面が先に肥大化する。

そのため、初期仕様では job API を持たない。

---

## 5. 全体アーキテクチャ

```text
Client
  ↓
CDN
  ↓
HTTP API Router
  ↓
Request Validation Layer
  ↓
Source Resolver
  ↓
Cache Layer
  ↓
Transform Engine
  ↓
Storage / Origin
```

主要コンポーネント:

- `Router`: endpoint と認証方式を振り分ける
- `Request Validation Layer`: signed URL、Bearer 認証、query、body を検証する
- `Source Resolver`: path / url / upload を統一的に解決する
- `Cache Layer`: origin response cache と transform cache を扱う
- `Transform Engine`: image / svg 変換を実行する

---

## 6. 変換モデル

用途:

- リサイズ
- contain / cover / fill / inside
- 画像フォーマット変換
- EXIF 自動回転
- メタデータ制御
- SVG の安全な再配信またはラスタ化

options:

- `width`
- `height`
- `fit`
- `position`
- `format`
- `quality`
- `background`
- `rotate`
- `autoOrient`
- `stripMetadata`
- `preserveExif`

共通ルール:

- `width` と `height` の両方があり `fit` が省略された場合は `contain`
- `position` の既定値は `center`
- `format` がある場合は `Accept` を無視する
- `quality` は lossy 出力形式だけで受け付ける

---

## 7. セキュリティ設計

### 7.1 認証モデル

- public GET API は signed URL を必須とする
- private API は `Authorization: Bearer` を必須とする
- `X-API-Key` は採用しない

### 7.2 signed URL canonical

canonical form は以下とする。

```text
METHOD + "\n" + AUTHORITY + "\n" + REQUEST_PATH + "\n" + CANONICAL_QUERY_WITHOUT_SIGNATURE
```

### 7.3 path 入力

- `percent_decode -> canonicalize -> validate_segments -> enforce_storage_root`
- `.` と `..` を拒否
- storage root 外への脱出を拒否

### 7.4 Origin Pull

- `http` / `https` のみ
- 許可ポートは `80` / `443`
- redirect は最大 `5` hop
- DNS 解決時と接続時の IP 再検証
- blocked IP range を持つ
- 圧縮応答は `gzip` / `br` のみ
- 展開後サイズ上限あり

### 7.5 メディア型検証

- Content-Type や拡張子を信用しない
- magic number で実 media type を判定する
- 宣言型と実データ型が違えば拒否する

### 7.6 SVG

- SVG は sanitize 必須
- `script`、外部参照、`foreignObject`、イベント属性を除去
- `data:` URL 経由の script 実行も除去

---

## 8. キャッシュ設計

### 8.1 キャッシュ階層

```text
Client
  ↓
CDN
  ↓
Origin Response Cache
  ↓
Transform Cache
  ↓
Storage
```

### 8.2 キャッシュキー

```text
SHA256(
  canonical_source_identifier + "\n" +
  canonical_transform_parameters + "\n" +
  normalized_accept_if_negotiation_enabled_and_format_absent
)
```

要件:

- `keyId`, `expires`, `signature` は除外する
- `format` がある場合は `Accept` をキーに含めない
- `format` がない場合だけ `Accept` を正規化してキーに含める
- `version` は source identity に含める

### 8.3 TTL と更新戦略

- `default_ttl_seconds = 3600`
- `max_ttl_seconds = 86400`
- revalidation を許可
- `stale-while-revalidate` を許可
- cache invalidation は `versioned source` を基本とする

### 8.4 保存レイアウト

```text
ab/cd/ef/<sha256>
```

### 8.5 レスポンスヘッダ

- `Cache-Control`
- `ETag`
- `Age`
- `Vary`
- `Cache-Status`
- `X-Content-Type-Options`
- `Content-Disposition`
- SVG 出力時のみ `Content-Security-Policy`

`Cache-Status` を採用し、`X-Cache` は使わない。

---

## 9. 実行制御

### 9.1 single-flight

同一 `cache_key` の変換は論理的に 1 回だけ実行する。

目的:

- 同一変換の重複実行防止
- hot object に対する CPU 増幅抑制
- cache miss 時のスパイク抑制

### 9.2 ワーカープール

- `image` queue: image / svg

### 9.3 backpressure

- `max_concurrent_transforms = 64`
- `queue_size = 1000`
- 超過時は `503 Service Unavailable`

---

## 10. 実装方針

画像:

- `image` crate を使ったネイティブ処理を基本とする

SVG:

- `resvg` による描画・変換を基本とする
- sanitize 後の SVG を入力として扱う

ストレージ:

- `path` 入力用の内部ストレージ
- Origin Pull 用の HTTP

---

## 11. 運用 API

- `/health`: 外形監視向けの簡易状態
- `/health/live`: プロセス生存確認
- `/health/ready`: readiness
- `/metrics`: Prometheus 互換、Bearer 認証必須

---

## 12. 初期仕様で守る原則

1. image-first を崩さないこと
2. source kind を仕様上も明確に分離すること
3. `format > Accept` を崩さないこと
4. option 名を CLI / library と揃えること
5. 同一変換は single-flight で 1 回だけ実行すること
6. path、origin、media type、SVG の検証を実装依存にしないこと
7. job queue など初期スコープ外の設計を先回りで入れないこと
