[English](run-directory.md) | **日本語**

# run ディレクトリ

1 回の実行が 1 つのディレクトリであり，そのディレクトリが記録である．その上にあるものは何一つ正本ではない．

```
<results_root>/<experiment>/<run_slug>/
├── run.json          ← run のメタデータ（同一性・code・env・data・lineage・research）
├── config.json       ← エンベロープ．条件は ["parameters"] の下に入る
├── metrics.csv       ← long 形式: run_uid, step, step_unit, scope, name, value
├── reference.csv     ← 原論文が報告している値．比較のため
├── events.jsonl      ← 観測単位ごとに 1 行
├── status.json       ← run がどう終わったか，いつ終わったか
├── manifest.csv      ← run が書いた全ファイルの同一性
├── lock/             ← 環境を固定した lock ファイルのコピー
├── logs/             ← run のログ
└── artifacts/        ← 実行中に実験が書き出したもの
```

`<results_root>` の既定値は `results`．experiment ディレクトリには最後に完了した run を指す `latest_finished` シンボリックリンクが置かれ，実行中の run は `.runvault.lock` を持つ．

## 各ファイル

### `run.json`

メタデータ．`run_uid`，`run_slug`，`repo_id`，`experiment`，`subcommand`，`domain`，3 つのハッシュ，`created_at`，`origin`，`visibility`，および `code` / `env` / `rng` / `llm` / `data` / `lineage` / `research` / `ext` の各ブロックを持つ．仕様は `schema/v1/run.json`．

2 つの項目は注意に値する．`data` は run が使ったデータセットで，空配列は「使っていない」を意味する —— 「記録していない」とは区別される．各エントリが `hash` / `dataset_id` / `uri` のいずれかを必要とするのはそのためである．そして `research` は run を再現対象の研究に結びつける．原論文と，その中の具体的な表・図のターゲットである．

### `config.json`

素のパラメータファイルではなく，エンベロープである．実験条件は `parameters` の下に入り，その隣に `runvault` 制御ブロックが並ぶ．ここで宣言するのは：

- `hash_exclude` —— すべてのハッシュから除くポインタ（`/output_dir`，`/log_level`）
- `seed_pointers` —— seed の在り処．複製が条件を共有しつつ実行だけを違えられるようにする
- `determinism.invariant_to` —— 結果を変えないと実験が *宣言する* ポインタ
- `sync_include` / `sync_exclude` —— 集約リポジトリへのコピーに加える／から外すもの

`invariant_to` は宣言するものであり，推測してはならない．たとえば `/threads` を無条件に除外すると，実際には結果が異なる run を 1 つの条件として束ねてしまう．

### `metrics.csv` と `reference.csv`

long 形式で，1 行 1 数値：`run_uid, step, step_unit, scope, name, value`．`step` を持たないメトリクスは run 全体を表す 1 つの数値（既定の scope は `run`），`step` を持つものは時間軸の上に乗る．

`reference.csv` は同じ列に `target_id` と `source` を加えたもので，原論文が報告している値を保持する．再現値と原論文値の差を，記憶に頼らず後から計算できるようにするためである．`source` はその数値をどこから読んだかを記録し，`target_id` は `research.targets[]` で宣言済みのものでなければならない．

### `events.jsonl`

観測単位ごとに 1 行．`observation` や `terminal` を名乗るレコードは，その種別が意味する予約キーを実際に持っていなければならない．名前だけの terminal 行は書けない．

### `status.json`

run がどう終わったか，いつ始まりいつ終わったか，そして各種カウント．finish されずに落ちた run は自らを失敗として記録する．

### `manifest.csv`

run が書いた全ファイルについて `run_uid, path, algorithm, digest, bytes`．

これは `finish()` によって **一度だけ** 書かれ，`finish()` が歩くのは `artifacts/` と `logs/` の 2 本だけである．ここから 2 つの帰結が出る．どちらも実務で効く：

- run ディレクトリの他の場所に書いたものは記録の一部にならない．
- `finish()` の後に足したものは manifest に入らない．したがって後から描いた図は run の *中* ではなく *隣* に置くべきである．

### `lock/`

リポジトリルートで見つかった lock ファイル —— `Cargo.lock`，`uv.lock`，`poetry.lock`，`requirements.lock` —— のコピー．それぞれハッシュ化されて `env.locks[]` に入り，`env_hash` に効く．ディレクトリ名は lock のハッシュに依存するハッシュから作られるので，記述が先，コピーはディレクトリができた後になる．

### `artifacts/` と `logs/`

生成物とログの置き場所．`finish()` が歩く 2 本であり，`runvault sync` が意図的に置いていく 2 本でもある —— `manifest.csv` が既にその同一性を持っているからである．

`logs/progress.log` は `run.stage(...)` が書く．標準エラーへ出したものと同じ行をここに残すので，サブコマンドがどれだけかかり，途中でどこにいたかが，起動したターミナルより長く残る．

## 生存期間と失敗

- 実行中の run は `.runvault.lock` を持ち，heartbeat で更新される．
- `finish()` されずに落ちた run は自らを失敗として記録する．
- プロセスが *kill* された run は，失敗を書く主体がいないまま lock だけを残す．`runvault gc` がこれを記録済みの失敗に変える．**lock があることだけでは「実行中」を意味しない．**
- `latest_finished` は前にしか進まない．長い run が短い run を追い越しても，リンクが後ろに戻ることはない．比較と置換はディレクトリ単位の mutex の下で行われるので，同時に終わった 2 つの run のうち古い方が最後に居座ることもない．

## sweep

sweep の親は 1 つの seed ではなく seed の列によって駆動される．親は sweep parent として宣言し，master seed は設定せず，`runvault` が `lineage.sweep_id` を run 自身の slug で埋める —— slug はハッシュを含み，ハッシュは run の開始時にしか計算できないので，呼び出し側はそれを知り得ない．子はそれを読み返し，各自の seed を持つ．

親は自分自身のメトリクスを持たない．`runvault path --latest` だけでは空のものが返ってくることがあるのはこのためで，[コマンドライン](cli.ja.md) の `--subcommand` / `--standalone` / `--children-of` を参照．
