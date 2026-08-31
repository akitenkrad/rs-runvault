[English](preservation.md) | **日本語**

# 記録を 1 台のマシンの外に出す

再現リポジトリは `results/` を ignore するので，コピー以外に記録がどこかへ出て行く経路はない．run ディレクトリから，問い合わせ可能で引用可能な記録に至る道は 4 つのコマンドである：`verify` → `sync` → `query` → `report`．

## `verify`

`runvault verify <run>` は run のファイルをまたぐ不変条件を検査し，`--deep` は加えて 3 つのハッシュを再計算し，artifacts を再ハッシュし，`events.jsonl` を歩く．`sync` はコピー前に deep 検査を走らせ，通らない run は送らない．集約層が壊れた run を受け入れることがないようにするためである．[検査](checks.ja.md) を参照．

## `sync`

```bash
runvault sync --repo-id R --vault <path> --dry-run   # 何が渡るか
runvault sync --repo-id R --vault <path>             # コピーする
```

コピーされるのは **軽い側** である．条件・結果・環境・来歴を再構成するファイル群 —— `run.json`，`config.json`，`status.json`，`metrics.csv`，`reference.csv`，`manifest.csv`，`events.jsonl` —— に加えて `lock/`，そして run 自身の `sync_include` / `sync_exclude` グロブが足し引きするもの．

`artifacts/`，`logs/`，`snapshots/`，`figures/` はその場に残る．`manifest.csv` が既にその同一性を持っているからであり，判断基準はファイルの名前ではなく **何であるか** だからである．ステップごとの格子ダンプは，`.npy` で書かれていようと `.csv` で書かれていようと記録の重い側である．legacy run が寄与するのは，読み手がまだ使える小さなテキストファイル —— `json`，`jsonl`，`csv`，`tsv`，`txt`，`md`，`yaml`，`yml`，`toml` —— だけで，図・チェックポイント・pickle は正規 run の `artifacts/` と全く同じように置いていかれる．

コピーはコピーである．コピー元の run ディレクトリには一切触れない．コピー先の各 run ディレクトリには `sync.json` の受領証が残る．

### コピー先は自分が何であるかを宣言しなければならない

ルートに `visibility = "private"` を宣言する `runvault-vault.toml` が無ければ，コマンドは推測せずに **停止する**．run の `events.jsonl` にはプロンプト・キャプチャ・内部データの断片が入りうるし，git の履歴は忘れてくれない．したがって失敗は寛容側ではなく閉じる側に倒してある．

```toml
schema_version = "1.0"
visibility     = "private"
# compress_over_mib = 10
# allow_internal    = false
```

public を宣言していない run を送るには `--allow-internal`（またはコピー先の `allow_internal = true`）が要る．`verify --deep` に通らない run はそもそも送られない．

## `query`

```bash
runvault query --vault <path> --refresh
runvault query --vault <path> "SELECT … FROM 'index/runs.parquet'"
```

`--refresh` は集約リポジトリを歩き，`index/` に 7 つの parquet テーブルを書く．列は `schema/v1/index.columns.json` が定義する：

| テーブル | 1 行が表すもの |
| --- | --- |
| `runs` | run |
| `run_data` | run が使ったデータセット |
| `run_targets` | run が宣言した再現ターゲット |
| `run_jira` | run が参照する課題キー |
| `metrics` | 記録された数値 |
| `reference` | 原論文の報告値 |
| `manifest` | run が書いたファイル |

インデックスは派生物である．git 管理されておらず，削除しても走査のコストしか掛からない．列定義は転記ではなく `index.columns.json` から読まれるので，SQL の例には存在するのに writer には無い列，という事態は起こらない．

本仕様より前に書かれた run も他と並べてインデックスされる．キーは `legacy:<repo_id>:<path>` で，`run_uid IS NULL` を持つ．埋められない列に何かを捏造することはない．

## `report`

```bash
runvault report --vault <path> --obsidian -o runs.json
```

インデックスをダッシュボード用ペイロードに要約する．形は `schema/v1/runs.report.json` が固定する．これは記録ではなく要約であり，失っても掛かるのはコマンド 1 回である．

記録していない run について何かを埋めることはない．legacy run には `status.json` が無いので，その state は `unfinished` ではなく `null` になる．後者は「本仕様に沿って書かれたが `status.json` を持たない run」を意味し，「そもそも知らされていない」とは別物である．
