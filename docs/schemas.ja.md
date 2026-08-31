[English](schemas.md) | **日本語**

# スキーマとテストベクタ

**`schema/v1/` が仕様である．** 実装がそれらのファイルに合わせるのであって，逆ではない．Rust リファレンスも Python 第 2 実装も同じファイルを読み，どちらかがずれれば CI が落ちる．

## ファイル

| ファイル | 固定するもの |
| --- | --- |
| `common.json` | 共有定義 —— slug，ハッシュ，タイムスタンプ |
| `run.json` | `run.json`: 同一性，`code`，`env`，`rng`，`llm`，`data`，`lineage`，`research` |
| `config.json` | config エンベロープと，その中の `runvault` 制御ブロック |
| `metrics.row.json` | `metrics.csv` の 1 行 |
| `reference.row.json` | `reference.csv` の 1 行 |
| `manifest.row.json` | `manifest.csv` の 1 行 |
| `event.json` | `events.jsonl` の 1 行 |
| `status.json` | `status.json` |
| `sync.json` | 同期先の run ディレクトリに残る受領証 |
| `vault.config.json` | 集約リポジトリの宣言 `runvault-vault.toml` |
| `index.columns.json` | 7 つの parquet テーブルの平坦化された列定義 |
| `index.columns.meta.json` | `index.columns.json` の形を保つメタスキーマ |
| `runs.report.json` | `runvault report --obsidian` が書くダッシュボード用ペイロード |
| `vocabulary.toml` | コア語彙．`vocab_version` で版管理される |

`vocabulary.toml` は閉じた値集合を固定する —— domain，データの role，メトリクスの scope，step の単位，イベント種別，予約メトリクス名．実験固有の語は名前空間付き（`x.<repo_id>.<name>`）にして，コア語彙と衝突させない．Python パッケージはリポジトリから離れた場所にインストールされるので自分用のコピーを同梱しており，`tools/sync_vocabulary.py` がそれを写し，テストが両者をバイト単位で一致させ続ける．

## テストベクタ

`schema/v1/testvectors/` は，2 つの実装が黙って食い違いうる箇所についての実装間ベクタを持つ：

| ファイル | 固定するもの |
| --- | --- |
| `canonicalize.json` | キーの順序，NFC，エスケープ，浮動小数点の書式，欠損と `null` の区別 |
| `length_prefix.json` | 入力列を曖昧にしないためのフレーミング |
| `hashes.json` | `env_hash` → `config_hash` → `execution_hash`．データ無し・コード無し・lock 無しの退化ケースを含む |

すべての実装は，入力だけから各ケースの **すべての** フィールドを再現できなければならない．中間の `canonical` / `joined` フィールドがあるのは，不一致が「ハッシュが違う」だけでなく *どこで* 分岐したかを示すためである．

**これらはテストの入力であって，生成物ではない．** 生成しているのは `tools/gen_testvectors.py` —— 仕様から書き起こした正規化とハッシュ規則の第 2 実装 —— であるが，**テストを通すために再生成してはならない**．ジェネレータと Rust リファレンスが食い違ったなら，どちらかが誤っている．ベクタを書き換えれば，どちらが誤っていたかが隠れる．変更してよいのは仕様が変わったときだけで，そのときはまず `schema/v1` を変える．

ベクタが `schema/v1/*.json`（非再帰のグロブ）の外に置かれているのは意図的で，スキーマバリデータがこれらを JSON Schema として読もうとしないためである．

## CI が守らせていること

ジョブは 3 つで，それぞれ「記録が静かに腐る」特定の経路を塞いでいる．

**Rust** —— `cargo fmt --all --check`，`cargo clippy --all-targets --all-features -- -D warnings`，`cargo test --all-features`．`schema-gen` feature は Rust の型から JSON Schema を出力し，テストが `schema/v1/*.json` と比較できるようにする．これが Rust の struct と仕様の乖離を止める．

**スキーマ** —— `tools/test_schemas.py` が，スキーマが何を受け入れ何を拒むべきかを固定する．続いてベクタを再生成し，`git diff --exit-code schema/v1/testvectors/` が差分で落ちる．Python 側から再生成して 1 バイトでも変わるなら，実装を直す代わりにベクタを書き換えた者がいる，ということである．

**Python** —— `python/src/runvault/models/` の Pydantic モデルを `tools/gen_pydantic.py` で再生成し，`git diff --exit-code` が差分で落ちる．モデルは `schema/v1` の *ビュー* であり，コミットしたうえで CI で再生成することが，ビューの乖離を止める．「生成できる」と「一致している」は別物である．その後 `pytest -q` が Python のテストを走らせる．

ローカルでの実行方法は [検査](checks.ja.md) を参照．
