# Truss CLI 設計

## 1. 目的

CLI は `Truss` ライブラリの基準 adapter である。  
ローカルで画像を変換できるだけでなく、library API の使い勝手が妥当かを検証するための入口でもある。

---

## 2. 設計方針

### 2.1 1 つの主要コマンドに寄せる

変換の主入口は `convert` に統一する。  
画像変換のたびに別 subcommand を増やさない。

### 2.2 API と同じ概念を同じ名前で出す

CLI flag は HTTP API の option 名を `kebab-case` に変換したものに揃える。

例:

- `width` -> `--width`
- `autoOrient` -> `--auto-orient`
- `stripMetadata` -> `--strip-metadata`

### 2.3 短縮オプションは最小限にする

短縮形を増やすと覚えやすく見えるが、意味の衝突が増える。  
初期 CLI では以下だけを短縮形にする。

- `-o`, `--output`

`-h` は help に使うため、高さ指定に流用しない。

### 2.4 出力先は明示させる

`convert` では `--output` を必須にする。  
暗黙のファイル名生成は便利そうに見えて、上書き事故や拡張子の混乱を起こしやすい。

標準出力に出す場合だけ `-o -` を許可する。

---

## 3. コマンド構成

## 3.1 `truss convert`

画像または SVG を変換するメインコマンド。

入力:

- `truss convert <INPUT>`: ローカルファイルまたは `-`（stdin）
- `truss convert --url <URL>`: リモート URL

基本形:

```sh
truss convert input.jpg -o output.webp --width 1200 --format webp
```

推奨オプション:

- `--width <PX>`
- `--height <PX>`
- `--fit <contain|cover|fill|inside>`
- `--position <center|top|right|bottom|left|top-left|top-right|bottom-left|bottom-right>`
- `--format <jpeg|png|webp|avif|svg|gif>`
- `--quality <1-100>`
- `--background <RRGGBB|RRGGBBAA>`
- `--rotate <0|90|180|270>`
- `--auto-orient`
- `--no-auto-orient`
- `--strip-metadata`
- `--keep-metadata`
- `--preserve-exif`

制約:

- `INPUT` と `--url` は排他
- `--preserve-exif` は `--keep-metadata` と併用時のみ有効
- `--quality` は lossy 出力形式でのみ有効
- `--position` は `--width` と `--height` の両方がある場合だけ意味を持つ

### 3.2 `truss inspect`

入力画像のメタデータを JSON で出力する。

入力:

- `truss inspect <INPUT>`
- `truss inspect --url <URL>`

出力例:

```json
{
  "format": "jpeg",
  "mime": "image/jpeg",
  "width": 3024,
  "height": 4032,
  "hasAlpha": false,
  "isAnimated": false
}
```

`inspect` は変換前の確認や、library 実装の妥当性確認に使う。

### 3.3 `truss serve`

server runtime を起動する。

初期オプション:

- `--bind <ADDR>`
- `--storage-root <PATH>`
- `--public-base-url <URL>`
- `--allow-insecure-url-sources`

変換オプションを `serve` に直接持ち込まない。  
`serve` は transport と設定だけを扱う。

`--allow-insecure-url-sources` はローカル開発や integration test 向けの escape hatch であり、
本番向け設定では使わない前提とする。

---

## 4. オプション設計

### 4.1 初期オプション一覧

| 意味 | CLI | HTTP query / JSON | 既定値 |
| --- | --- | --- | --- |
| 出力幅 | `--width` | `width` | なし |
| 出力高さ | `--height` | `height` | なし |
| リサイズ方式 | `--fit` | `fit` | `contain` |
| 配置位置 | `--position` | `position` | `center` |
| 出力形式 | `--format` | `format` | `Accept` または入力形式 |
| 品質 | `--quality` | `quality` | backend 既定 |
| 背景色 | `--background` | `background` | なし |
| 回転 | `--rotate` | `rotate` | `0` |
| 自動回転 | `--auto-orient` / `--no-auto-orient` | `autoOrient` | `true` |
| メタデータ除去 | `--strip-metadata` / `--keep-metadata` | `stripMetadata` | `true` |
| EXIF 保持 | `--preserve-exif` | `preserveExif` | `false` |

### 4.2 `crop` を初期 CLI に入れない理由

矩形 crop は一見分かりやすいが、以下で複雑化しやすい。

- 座標系の原点
- EXIF 向き補正との関係
- SVG の viewBox との関係
- リサイズと crop の評価順

そのため、初期 CLI は `fit` と `position` に絞る。

### 4.3 真偽値は対になる flag を用意する

真偽値は `--foo` と `--no-foo`、または `--strip-metadata` と `--keep-metadata` のように明示する。  
`--foo=false` 形式を基本にしないことで、help とシェル補完を読みやすくする。

---

## 5. 使い勝手の原則

### 5.1 stdin / stdout を扱えるようにする

- `INPUT` に `-` を指定すると stdin を読む
- `-o -` を指定すると stdout に書く

これにより、他ツールとのパイプ連携がしやすくなる。

### 5.2 失敗は黙って補正しない

以下は自動補正せず、原則 `2` で終了する。

- 矛盾するオプションの併用
- 未対応出力形式への `quality` 指定
- `--preserve-exif` と `--strip-metadata` の併用

### 5.3 CLI だけの別名を増やしすぎない

たとえば `jpg` を `jpeg` の別名として受けるかどうかは実装時に判断してよいが、help やドキュメントの正規名は `jpeg` に統一する。

---

## 6. 終了コード

- `0`: 成功
- `2`: 入力やオプション不正
- `3`: 入力画像の読み込み失敗
- `4`: 変換失敗
- `5`: 出力書き込み失敗

---

## 7. 例

ローカルファイルを変換:

```sh
truss convert input.jpg -o output.avif --width 1600 --quality 70
```

cover でサムネイル化:

```sh
truss convert input.png -o thumb.webp --width 320 --height 320 --fit cover --position center
```

URL 入力:

```sh
truss convert --url https://example.com/image.jpg -o result.webp --width 1200 --format webp
```

メタデータ確認:

```sh
truss inspect input.jpg
```
