# edstem-tui

[Ed Discussion](https://edstem.org) の **Lessons** をターミナルで読み、講義資料をダウンロードする TUI。
コース → モジュール → レッスン → スライドをツリーでたどり、スライドの本文をその場で表示する。

- Vim ライクなキー操作（ratatui + crossterm）
- Ed の XML 形式の本文を、見出し・リスト・コード・表・数式などに分けて描画する
- PDF スライドや本文中の添付ファイルを、コース・レッスンごとのフォルダに保存する
- **読み取り専用**。Ed 側の既読やスライドの進捗は変えない

Ed の API は非公式なので、Ed 側の変更で動かなくなることがある。

## 必要なもの

| 用途 | 必要なもの |
| --- | --- |
| ビルド | Rust（edition 2021） |
| 認証 | Ed の API トークン |

## インストール

```bash
git clone git@github.com:shurto11/edstem-tui.git ~/ssd/tui/edstem-tui
cd ~/ssd/tui/edstem-tui
cargo install --path .     # ~/.cargo/bin/edstem-tui に入る。更新も同じコマンド
```

## API トークン

1. Ed の設定画面でトークンを作る
   - US: https://edstem.org/us/settings/api-tokens
   - AU: https://edstem.org/au/settings/api-tokens
2. 保存する（入力は画面に出ない）

```bash
mkdir -p ~/.config/edstem-tui && read -rs t && printf '%s\n' "$t" > ~/.config/edstem-tui/token && chmod 600 ~/.config/edstem-tui/token
```

トークンは次の順で探す。

1. 環境変数 `EDSTEM_TOKEN`
2. `~/.config/edstem-tui/token`
3. `~/.config/edstem-cli/token`

## 起動

```bash
edstem-tui
edstem-tui --region au                 # リージョンを指定する
edstem-tui --download-dir ~/Downloads  # 保存先を変える
```

| オプション | 内容 |
| --- | --- |
| `--region <us\|au>` | Ed のリージョン。省略すると前回つながったリージョン → US → AU の順に試し、つながったものを `~/.config/edstem-tui/region` に覚える |
| `--download-dir <dir>` | ダウンロードの保存先（既定は `~/ssd/tui/edstem-tui/downloads`） |
| `-h`, `--help` | ヘルプ |
| `--version` | バージョン |

環境変数 `EDSTEM_BASE_URL`（例: `https://us.edstem.org/api/`）を設定すると、リージョンより優先して API の URL に使う。

## 画面

- **左**: コース → モジュール → レッスン → スライドのツリー。子は展開したときに取得する
  - 公開日時がまだ来ていないレッスンは灰色で `[locked]` と表示し、展開できない
  - スライドの種類は `[pdf]` `[quiz]` などのラベルで示す
- **右**: 選択中の項目の中身。レッスンなら状態・公開日時・締切・スライド一覧、スライドなら本文と添付ファイル
- **下**: キーのヘルプとステータス

フォーカス中のペインは枠が黄色になる。

## キー操作

矢印キーは使わない。`?` でいつでも一覧を出せる。

| キー | ツリー | 本文 |
| --- | --- | --- |
| `j` / `k` | 移動 | スクロール |
| `l` / `Enter` | 展開。スライドなら本文へ | |
| `h` / `Esc` | 折りたたむ / 親へ | ツリーへ戻る |
| `r` | 選択中の項目を再取得 | |
| `Ctrl+d` / `Ctrl+u` | 半ページ下 / 上 | 半ページ下 / 上 |
| `gg` / `G` | 先頭 / 末尾 | 先頭 / 末尾 |
| `Tab` | ペインを切り替える | ペインを切り替える |
| `d` | スライドの資料を保存（複数あれば選択画面）。レッスン上ではレッスン全体 | 同左 |
| `D` | 選択中のレッスンの資料をすべて保存 | 同左 |
| `q` / `Ctrl+c` | 終了 | 終了 |

## ダウンロード

- 対象は PDF スライドと、本文中の添付ファイル。Ed のファイル置き場（`*.edusercontent.com`）にあるものだけを保存し、トークンは送らない
- 保存先はツリーと同じ入れ子で `<保存先>/<コースコード>/<モジュール名>/<番号> <レッスン名>/`
  - モジュールに属さないレッスンはコース直下、番号の無いレッスンは名前だけになる
  - 名前の中の `/` は `_` に置き換える（例: `第1回: ガイダンス・導入 (10_1)`）
- ファイル名は Content-Disposition → 本文中のファイル名 → スライド名 + `.pdf` の順に決める
- 同じ名前のファイルがあれば ` (1)` のように番号を付け、上書きしない

## 開発

```bash
cargo test
cargo clippy --all-targets
```
