# Truss Image API 仕様

## 1. 概要

本 API は `Truss` の server runtime が提供する HTTP interface であり、画像と SVG をオンデマンドで変換して返却する。  
`Truss` 全体は library-first で設計し、CLI / server / WASM は共通ライブラリを利用する。全体方針は [`doc/runtime-architecture.md`](./runtime-architecture.md) を参照する。

- Base URL: `http://localhost:8080`
- Swagger/OpenAPI: `doc/openapi.yaml`
- 現行仕様バージョン: `0.8.0`

この版では、画像専用 API として形を整理し直した。

主な変更:

- 非同期 job API を初期スコープから外した
- private API に `POST /images:transform` を追加した
- `crop` と `orientation` を初期オプションから外し、`fit` / `position` / `rotate` に寄せた
- option 名を CLI と揃えやすい形に固定した

---

## 2. API 設計の原則

### 2.1 入口を用途ごとに分ける

同じ変換機能でも、呼び出し側の都合は 3 種類に分かれる。

- CDN 配下で安全に配布したい public GET
- サーバー間連携で JSON を送りたい private POST
- ファイルを直接投げたい upload POST

そのため、初期 API は以下に分ける。

- `GET /images/by-path`
- `GET /images/by-url`
- `POST /images:transform`
- `POST /images`

### 2.2 オプション名を揃える

同じ意味のオプションに別名を増やさない。

| 意味 | GET query / JSON | CLI |
| --- | --- | --- |
| 出力幅 | `width` | `--width` |
| 出力高さ | `height` | `--height` |
| リサイズ方式 | `fit` | `--fit` |
| 配置位置 | `position` | `--position` |
| 出力形式 | `format` | `--format` |
| 品質 | `quality` | `--quality` |
| 背景色 | `background` | `--background` |
| 回転 | `rotate` | `--rotate` |
| 自動回転 | `autoOrient` | `--auto-orient` |
| メタデータ除去 | `stripMetadata` | `--strip-metadata` |
| EXIF 保持 | `preserveExif` | `--preserve-exif` |

### 2.3 `crop` を初期 API に入れない

座標ベースの `crop` は API の複雑さに対して事故が多い。

- 向き補正との評価順が分かりにくい
- SVG と raster image で意味が揃いにくい
- CLI と query string の両方で可読性が落ちる

初期 API は `fit` と `position` に寄せ、よく使う resize を優先する。

### 2.4 `orientation` を公開しない

EXIF orientation 番号は user-facing API として分かりにくい。  
そのため、初期 API は `autoOrient` と `rotate` の組み合わせだけを公開する。

---

## 3. エンドポイント一覧

画像変換:

- `GET /images/by-path`
- `GET /images/by-url`
- `POST /images:transform`
- `POST /images`

運用:

- `GET /health`
- `GET /health/live`
- `GET /health/ready`
- `GET /metrics`

---

## 4. 入力モデル

### 4.1 public GET

公開 GET API は source kind ごとに endpoint を分離する。

- `.../by-path`: `path` を必須とする
- `.../by-url`: `url` を必須とする

これにより、`path` / `url` 排他を OpenAPI 上も明確にする。

追加の source identity:

- `version`: cache invalidation 用の source version

### 4.2 private JSON API

`POST /images:transform` は `application/json` を受け付ける。

```json
{
  "source": {
    "kind": "path",
    "path": "/products/hero.jpg",
    "version": "2026-03-08"
  },
  "options": {
    "width": 1200,
    "height": 630,
    "fit": "cover",
    "position": "center",
    "format": "webp"
  }
}
```

この endpoint は server-to-server で最も扱いやすい入口として用意する。

### 4.3 upload API

`POST /images` は `multipart/form-data` を使う。

共通構造:

- `file`: binary
- `options`: JSON object

例:

```bash
curl -X POST http://localhost:8080/images \
  -H "Authorization: Bearer <token>" \
  -F "file=@image.jpg" \
  -F 'options={"width":200,"format":"webp"};type=application/json'
```

---

## 5. 変換オプション

### 5.1 初期オプション

- `width`
- `height`
- `fit`: `contain`, `cover`, `fill`, `inside`
- `position`: `center`, `top`, `right`, `bottom`, `left`, `top-left`, `top-right`, `bottom-left`, `bottom-right`
- `format`: `jpeg`, `png`, `webp`, `avif`, `svg`, `gif`
- `quality`: `1` から `100`
- `background`: `RRGGBB` または `RRGGBBAA`
- `rotate`: `0`, `90`, `180`, `270`
- `autoOrient`: 既定値 `true`
- `stripMetadata`: 既定値 `true`
- `preserveExif`: 既定値 `false`

### 5.2 評価ルール

- `width` と `height` の両方があり `fit` が省略された場合は `contain`
- `position` の既定値は `center`
- `format` がある場合は `Accept` を無視する
- `format` がない場合だけ `Accept` negotiation を使ってよい
- `quality` は `jpeg` / `webp` / `avif` に対してのみ有効
- `preserveExif=true` は `stripMetadata=false` を必須とする
- `preserveExif=true` と `format=svg|gif` の組み合わせは `400`

### 5.3 SVG の扱い

- SVG input は sanitize を必須とする
- `format=svg` の場合は sanitize 済み SVG を返す
- raster format を指定した場合は rasterize して返す

---

## 6. 認証と署名

### 6.1 公開 GET API

以下は signed URL を必須とする。

- `GET /images/by-path`
- `GET /images/by-url`

必須 query:

- `keyId`
- `expires`
- `signature`

canonical form:

```text
METHOD + "\n" + AUTHORITY + "\n" + REQUEST_PATH + "\n" + CANONICAL_QUERY_WITHOUT_SIGNATURE
```

### 6.2 private API

以下は Bearer 認証を必須とする。

- `POST /images:transform`
- `POST /images`
- `GET /metrics`

認証方式:

```text
Authorization: Bearer <token>
```

---

## 7. セキュリティ要件

### 7.1 path 入力

- percent-decode 後に canonicalize する
- `.` と `..` の単独セグメントを拒否する
- storage root 外に出る path を拒否する

### 7.2 Origin Pull

- `http` / `https` のみ許可
- 許可ポートは `80` / `443`
- 接続前に DNS 解決し、拒否 IP レンジを除外
- 接続時に IP を再検証する
- redirect ごとに再解決・再検証する
- redirect は最大 `5` hop
- origin response size は `100 MiB` を上限とする
- `gzip` / `br` 以外の圧縮応答は拒否
- 展開後サイズが `200000000 bytes` を超えたら中断

### 7.3 メディア型検証

- Content-Type や拡張子は信頼しない
- magic number で実 media type を判定する
- 宣言型と実 media type が不一致なら拒否する

### 7.4 SVG / GIF

- SVG は sanitize 必須
- `script`、外部参照、`foreignObject`、イベント属性を除去
- `data:` URL 経由の script 実行も除去対象
- GIF decompression bomb 対策として frame 数と総ピクセル数を制限する

---

## 8. キャッシュ設計

キャッシュ階層:

- `CDN`
- `origin_response_cache`
- `transform_cache`

キャッシュキー:

```text
SHA256(
  canonical_source_identifier + "\n" +
  canonical_transform_parameters + "\n" +
  normalized_accept_if_negotiation_enabled_and_format_absent
)
```

要件:

- `keyId`, `expires`, `signature` はキャッシュキーから除外
- `format` がある場合は `Accept` を無視する
- `format` がない場合だけ `Accept` を正規化して含める
- 幅と高さは `2px` 単位で正規化してもよい
- source ごとの variant 数は `128` を目安に抑制する

TTL:

- `default_ttl_seconds = 3600`
- `max_ttl_seconds = 86400`
- revalidation を許可する
- `stale-while-revalidate` を許可する

無効化:

- purge より `versioned source` を優先する
- version query は `version`

---

## 9. レスポンス方針

返却 MIME:

- `image/jpeg`
- `image/png`
- `image/webp`
- `image/avif`
- `image/svg+xml`
- `image/gif`

共通ヘッダ:

- `Cache-Control`
- `ETag`
- `Age`
- `Vary`
- `Cache-Status`
- `X-Content-Type-Options: nosniff`
- `Content-Disposition`

SVG 出力時のみ:

- `Content-Security-Policy: sandbox`

画像レスポンスでは Range Request を扱わない。

---

## 10. 変換制限

- `max_output_pixels = 67108864`
- `max_decoded_pixels = 100000000`
- `max_decode_cpu_seconds = 30`
- `gif.max_frames = 1000`
- `gif.max_total_pixels = 200000000`

---

## 11. 運用 API

- `/health`: サービス全体の簡易状態
- `/health/live`: プロセス生存確認
- `/health/ready`: リクエスト受け付け可否
- `/metrics`: Prometheus 互換、Bearer 認証必須
