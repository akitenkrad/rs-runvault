[English](python.md) | **日本語**

# Python

`python/` は Rust バイナリを薄く包んだクライアントではない．**完全な第 2 実装** である．同一性を計算し，run ディレクトリを書き，読み返す．Python から記録した run は，Rust リファレンスが書いたであろう run ディレクトリと同じものになる．

これは意図的である．`schema/v1/testvectors/` のベクタは，2 つの実装が独立にそこへ到達して初めて何かを証明する．したがってここのハッシュ原始関数は Rust 側に結び付けず，独立に保たれている．

## インストール

パッケージは本リポジトリの `python/` サブディレクトリにあり，import 名は `runvault`：

```toml
[project]
dependencies = ["runvault"]

[tool.uv.sources]
runvault = { git = "https://github.com/akitenkrad/rs-runvault", subdirectory = "python" }
```

writer が依存するのは `blake3` と `pydantic` だけである．`runvault.read` の解析ヘルパは pandas を必要とし，これは `read` extra の側にある：

```toml
dependencies = ["runvault[read]"]
```

run を記録するだけで解析はしないリポジトリに，pandas とその wheel を入れさせないためである．

## run を書く

```python
from runvault import Run

with Run.start(
    "schelling",                       # experiment
    "main",                            # subcommand
    repo_id="social-simulation-replications",
    domain="simulation",
    parameters=cfg,
    hash_exclude=["/output_dir", "/log_level"],
    seed_pointers=["/seed"],
    master_seed=42,
) as run:
    run.log_metric("segregation_index", 0.834, step=120, step_unit="step")
    run.log_metrics_at(120, "step", "group", {"share_a": 0.51, "share_b": 0.49})
    run.log_event("observation", record)
```

ブロックを正常に抜ければ run は finish する．例外で抜ければ，その例外を理由として失敗が記録される．そのどちらでもない run —— `finish()` を呼ばないままインタプリタが終了した run —— も失敗として自らを記録する．これを免れるのは kill されたプロセスだけで，そのために lock ファイルの heartbeat と `runvault gc` がある．

`RunOptions` が持つオプションはすべて `Run.start` にキーワード引数として渡せる．別途組み立てた `RunOptions` は `Run.from_options(options)` に渡す．

### 記録用メソッド

| 呼び出し | 記録するもの |
| --- | --- |
| `run.log_metric(name, value, *, step=None, step_unit=None, scope="run")` | 数値 1 つ |
| `run.log_metrics(scope, values)` | scope を共有する集約メトリクスを複数，flush は 1 回 |
| `run.log_metrics_at(step, step_unit, scope, values)` | 同じものを時間軸の上に |
| `run.log_reference(name, value, *, target_id, source, step=None, step_unit=None, scope="run")` | 原論文が報告している値 |
| `run.log_event(kind, payload)` | `events.jsonl` の 1 行 |

`log_metrics` があるのは，1 数値 1 呼び出しでは 1 数値 1 flush になるからである．長いシミュレーションの各ステップで数個のメトリクスを記録する run では，これが効いてくる．

`log_reference` には，その数値をどこから読んだかを示す `source` と，`research.targets[]` で宣言済みの `target_id` が要る．図から目分量で読んだ値をここに書いてはならない．推定値を報告値として記録すると，後から両者を区別できなくなる．

`run.dir`，`run.artifacts`，`run.run_uid`，`run.run_slug`，`run.meta`，`run.sweep_id` が，その run が結局何だったかを返す．

## run を読み返す

`runvault.read` はもう半分である．「どの run か」と「エンベロープをどう開くか」を 1 か所に集め，すべての解析スクリプトが両方について一致するようにする．runvault 以前のレイアウト —— 平坦な `config.json`，wide の `metrics.csv`，`run.json` なし —— も読む．既にディスク上にある結果は書き換えられないからである．

```python
from runvault import read

run_dir = read.runvault_path("schelling", subcommand="simulate", standalone=True)
params  = read.config_parameters(run_dir)
scores  = read.run_scope_metrics(run_dir)
events  = read.events_table(run_dir)
```

| 関数 | 返すもの |
| --- | --- |
| `runvault_path(experiment, results_root="results", subcommand=None, standalone=False)` | `runvault path --latest` 経由で，最後に完了した run |
| `runvault_binary()` | 上記が使う `runvault` 実行ファイル |
| `config_parameters(run_dir, *, required=True)` | エンベロープから取り出した条件 |
| `load_run_meta(run_dir, *, required=True)` | `run.json` |
| `run_subcommand(run_dir)` | その run がどのサブコマンドの実行だったか |
| `artifacts_dir(run_dir)` | run 自身の出力の置き場所 |
| `figures_dir(run_dir)` | *後から* 描く場所．run ディレクトリの外 |
| `metrics_wide(metrics_path)` | `metrics.csv` を wide に展開したもの |
| `run_scope_metrics(run_dir)` | run 全体を表す 1 数値のメトリクス群 |
| `scope_metrics_from_csv(metrics_path)` | 同じものを `metrics.csv` のパスから |
| `events_table(run_dir, kind="terminal")` | `events.jsonl` を DataFrame として |
| `sweep_children(parent_dir)` | sweep 親の子 run．`lineage.parent_run_uid` で対応付け |
| `sweep_summary_table(sweep_dir, parameter_keys, metric_names=None)` | 条件ごとに 1 行 |
| `sweep_events_table(sweep_dir, parameter_keys, kind="terminal")` | 試行ごとに 1 行 |

このうち 2 つは，解析スクリプトを書く前に知っておく価値のある規則を含んでいる．

- **`standalone=True`**．sweep の子は手で起動した run と同じサブコマンドを走らせるので，`subcommand="simulate"` だけでは *最後に走った子* が返ってくる．関心のある run が単発のものであれば，必ず `standalone=True` を付ける．
- **`figures_dir` は `artifacts_dir` ではない**．`manifest.csv` は `finish()` で確定する．run が終わった後に描いた図はハッシュを持たず，記録の一部ではない．そこで `figures_dir` はそれを run の隣 —— `<results_root>/<experiment>/figures/<run_slug>/` —— に置く．manifest と矛盾しようがない場所である．

`sweep_summary_table` と `sweep_events_table` は，runvault がディスク上に持たない表を，子の `config.json` と `metrics.csv` から組み立てる．各行が自分の `run_dir` を持つので，呼び出し側が条件からディレクトリ名を composite する必要はない．`sweep_children` は `runvault path --children-of` を呼ばず自前で親の隣を走査する．run を読むのにディレクトリ以外を要らなくするためで，バイナリがビルドされていない環境でも解析スクリプトが動く．

## Pydantic モデル

`python/src/runvault/models/` は `tools/gen_pydantic.py` により `schema/v1/*.json` から **生成** され，コミットされている．CI は再生成して差分が出れば失敗する．「生成できる」と「一致している」は別物だからである．スキーマを変えてから再生成する．逆順にはしない．

```bash
uv run --with datamodel-code-generator python tools/gen_pydantic.py
```

## テスト

```bash
cd python && uv run --group dev pytest -q
```

原始関数，同一性，writer，読み出し側，テストベクタ，スキーマ適合，そして Rust 実装が書いた fixture の読み取りまでを対象にしている．
