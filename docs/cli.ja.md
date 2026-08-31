[English](cli.md) | **日本語**

# コマンドライン

```bash
cargo install --git https://github.com/akitenkrad/rs-runvault runvault-cli
```

チェックアウトからなら `cargo build --release` でバイナリが `target/release/runvault` にできる．

```
runvault path     run ディレクトリを出力する: 最後に完了したもの，または条件を共有するもの
runvault verify   run ディレクトリを，ファイルをまたぐ不変条件に照らして検査する
runvault gc       プロセスが kill された run を，記録済みの失敗に変える
runvault legacy   本仕様より前に書かれた run ディレクトリを読む
runvault sync     各 run の軽い側を集約リポジトリへコピーする
runvault query    インデックスを再構築する，SQL を実行する，あるいは両方
runvault report   ダッシュボード用にインデックスを要約する
```

## 一覧

| | |
| --- | --- |
| `runvault path --experiment E --latest` | 最後に完了した run |
| `runvault path --experiment E --config-hash 9f2c41ab` | ある条件のすべての run |
| `runvault path --experiment E --execution-hash 3b1d --finished` | この全く同じものは既に実行済みか |
| `runvault path --experiment E --latest --subcommand run` | あるサブコマンドの最新の run |
| `runvault path --experiment E --latest --subcommand run --standalone` | …ただし sweep が起動したものは除く |
| `runvault path --experiment E --children-of <run_uid>` | ある sweep の子 run |
| `runvault verify <run>` | run のファイルをまたぐ不変条件 |
| `runvault verify <run> --deep` | …に加えてハッシュを再計算し，artifacts を再ハッシュし，`events.jsonl` を歩く |
| `runvault gc` | プロセスが kill された run を記録する |
| `runvault sync --repo-id R --vault V --dry-run` | 集約リポジトリが受け取る内容 |
| `runvault sync --repo-id R --vault V` | 各 run の軽い側をそこへコピーする |
| `runvault query --vault V --refresh` | そのリポジトリから `index/*.parquet` を再構築する |
| `runvault query --vault V "SELECT …"` | 全リポジトリ横断で問い合わせる |
| `runvault report --obsidian --vault V -o runs.json` | ダッシュボード用にインデックスを要約する |

## `path`

run ディレクトリを出力する．

| フラグ | 意味 |
| --- | --- |
| `--experiment <EXPERIMENT>` | 探す experiment（必須） |
| `--results-root <RESULTS_ROOT>` | experiment ディレクトリの置き場所（既定 `results`） |
| `--latest` | `latest_finished` リンクを解決する |
| `--config-hash <CONFIG_HASH>` | `config_hash` がこの接頭辞で始まる run すべて —— 同じ条件 |
| `--execution-hash <EXECUTION_HASH>` | `execution_hash` がこの接頭辞で始まる run すべて |
| `--finished` | 完了した run だけ．失敗した run は起きた run ではない |
| `--subcommand <SUBCOMMAND>` | このサブコマンドの run だけ |
| `--standalone` | どの sweep にも属さない run だけ |
| `--children-of <RUN_UID>` | この sweep 親の子だけ |

後ろの 3 つは sweep のためにある．sweep の親と子は experiment を共有するので，`--latest` だけでは自身のメトリクスを持たない親が返ってくることがある．また子は手で起動した run と同じサブコマンドを走らせるので，サブコマンドで絞るだけでは最後の子が返ってくる．

`--execution-hash … --finished` が「この全く同じものは既に実行済みか」に答える．同じ条件・同じ seed・同じ commit・同じ環境である．

## `verify`

```bash
runvault verify <RUN>
runvault verify <RUN> --deep
```

`--deep` なしなら run のファイルをまたぐ不変条件のみ．付ければハッシュを再計算し，artifacts を再ハッシュし，`events.jsonl` を歩く．deep のコストは run の大きさに比例するので，毎回の実行が終了時に行うものにはしていない —— ただし `sync` はコピー前に必ず走らせる．[検査](checks.ja.md) を参照．

## `gc`

```bash
runvault gc [--results-root <RESULTS_ROOT>] [--dry-run]
```

プロセスが kill された run を記録済みの失敗に変える．`--dry-run` は何も書かずに，何が起きるかだけを報告する．

## `legacy`

```bash
runvault legacy --repo-id <REPO_ID> [--results-root <RESULTS_ROOT>] [--json] [--notes]
```

本仕様より前に書かれた run ディレクトリを読む．`--json` は要約ではなく JSON で出力し，`--notes` は各 run が変換できなかったものも出力する．legacy run が埋められない項目を捏造することはない．

## `sync`

```bash
runvault sync --repo-id <REPO_ID> --vault <VAULT> [--dry-run] [--allow-internal]
```

| フラグ | 意味 |
| --- | --- |
| `--repo-id <REPO_ID>` | 安定したリポジトリ id．`run.json` がこれと食い違う正規 run は，推測ではなくエラーになる |
| `--vault <VAULT>` | 集約リポジトリ．private を宣言していなければならない |
| `--results-root <RESULTS_ROOT>` | experiment ディレクトリの置き場所（既定 `results`） |
| `--dry-run` | 何も書かずに，何がどれだけコピーされるかを列挙する |
| `--allow-internal` | public を宣言していない run も送る |

何がコピーされ，コピー先が何を宣言していなければならないかは [保全](preservation.ja.md) を参照．

## `query`

```bash
runvault query --vault <VAULT> --refresh
runvault query --vault <VAULT> "SELECT experiment, count(*) FROM 'index/runs.parquet' GROUP BY 1"
```

インデックスを再構築する，SQL を実行する，あるいは両方．テーブルは parquet のパス —— `index/<name>.parquet` —— として指定する．

## `report`

```bash
runvault report --vault <VAULT> --obsidian -o runs.json
```

インデックスを要約する．`--obsidian` はダッシュボードが読むペイロードを書き，`-o`/`--out` は出力先を指定する（既定は標準出力）．
