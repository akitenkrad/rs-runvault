[English](rust.md) | **日本語**

# Rust

```toml
[dependencies]
runvault = { git = "https://github.com/akitenkrad/rs-runvault" }
```

run を書くためにライブラリへ依存しても，データベースは一切入らない．

## run を記録する

```rust
use runvault::{Run, RunOptions, Target, Work};

let mut run = Run::start(
    RunOptions::new("schelling", "main")          // experiment, subcommand
        .repo_id("social-simulation-replications")
        .domain("simulation")
        .parameters(&cfg)?
        .hash_exclude(["/output_dir", "/log_level"])
        .seed_pointers(["/seed"])
        .master_seed(42)
        .replication(
            Work::doi("10.1080/0022250X.1971.9989794")
                .title("Dynamic Models of Segregation")
                .source_version("published")
                .target(Target::table("tbl3-r2", "Table 3").row("2"))
                .obsidian_note("notes/replications/segregation.md"),
        ),
)?;

run.log_metric("segregation_index", 0.834).step(120, "step").send()?;
run.log_reference("segregation_index", 0.850)
    .target("tbl3-r2")
    .source("Table 3 row 2")
    .send()?;
run.log_event("observation", &record)?;
run.finish()?;
```

最小構成はもっと短い：

```rust
use runvault::{Run, RunOptions};

let cfg = serde_json::json!({ "rows": 13, "cols": 16, "seed": 42 });
let mut run = Run::start(
    RunOptions::new("schelling", "main")
        .repo_id("social-simulation-replications")
        .domain("simulation")
        .parameters(&cfg)?
        .seed_pointers(["/seed"])
        .master_seed(42),
)?;
run.log_metric("segregation_index", 0.834).step(120, "step").send()?;
run.finish()?;
```

`log_reference` は原論文が報告している値を記録する．再現値との差を，記憶ではなく計算で後から出せるようにするためである．`target` は replication で宣言済みのターゲットでなければならない．

## `finish()` が記録するもの

生成ファイルは `run.dir()/artifacts/` の下に，ログは `logs/` の下に書く．`finish()` はまさにこの 2 本を歩いて `manifest.csv` を作るので，run ディレクトリの他の場所に書いたものは記録の一部にならない．

`manifest.csv` は `finish()` によって一度だけ書かれる．その後に run ディレクトリへ足したものは manifest に入らないので，後から描いた図は run の中ではなく隣に置くべきである．

## 進捗

1 分を超えて走りうるサブコマンドは，自分が何をしているかを報告する．長い沈黙が仕事なのか停止なのかは run ディレクトリの他のどこにも書かれておらず，`ps` で見分けるのはプログラムの外で下す診断である．

```rust
let mut stage = run.stage("stage 2", conditions.len());
for condition in &conditions {
    let value = evaluate(condition);
    run.log_metric("segregation_index", value).send()?;
    stage.tick();
}
stage.close();
```

```
progress: stage 2          200/4000     5%  elapsed      12s  eta    3m54s
progress: stage 2         4000/4000   100%  elapsed    4m06s  done
```

行は **標準エラー** へ出る．標準出力は run の機械可読な結果のために空けてある．同じ行が `logs/progress.log` にも残り，`finish()` がそれを `manifest.csv` にハッシュする．行ごとに flush し，`isatty` は決して見ない —— 出力がリダイレクトされている run こそ，進捗が要る run である．刻みは総数の 5% ごと，かつ遅くとも 30 秒ごとで，早く来た方が採用される．

呼び出し側は「どれだけ仕事があるか」と「1 件終わった」だけを言う．行を組み立てず，ストリームを選ばず，いつ報告するかも決めない．`Stage` は何も借用しないので，報告している当のループの中で run が指標を記録できる．

数え上げでは表せない段には 2 つの変種がある：

| 呼び出し | 用途 |
| --- | --- |
| `run.weighted_stage(name, costs)` | 条件ごとにコストが違う段．`costs` は tick 順に 1 条件 1 個，時間に比例する任意の単位で与える．割合と見積りは件数ではなくコストの取り分になる．コストが桁で違う段を件数で数えると，残り 30 分の地点で「19s」と言い切る． |
| `run.unbounded_stage(name)` | 先に数えられない仕事．分母を捏造せず，到達した件数を一定時間ごとに報告し，割合も見積りも持たない． |

進捗は指標ではない．`metrics.csv` が持つのは実験が測ろうとしている量であり，run にかかった時間は `status.json` の `duration_sec` である．同じ問いへの 2 つ目の答えは，食い違いうる 2 つ目の答えである．

段は `finish()` より前に閉じる．manifest はそこで書かれるので，その後に足した行は manifest が食い違う行になる —— run の終わりを越えて開いたままの段は，標準エラーへは報告を続け，そのことを 1 度だけ言い，ディレクトリへの書き込みだけをやめる．

## sweep

sweep の親は 1 つの seed ではなく seed の列で駆動されるので，`RunOptions::sweep_parent()` で宣言し（これが `lineage.sweep_id` を run 自身の slug で埋める），`master_seed` は設定しない．子はそれぞれ自分の seed を持つ．

## 失敗

`finish()` されずに落ちた run は自らを失敗として記録する．プロセスが kill された run は lock を残し，`runvault gc` がそれを記録済みの失敗に変える —— lock があることだけでは決して「実行中」を意味しない．

## 2 つの crate

| crate | 何であるか |
| --- | --- |
| `crates/runvault` | ライブラリ．run の書き出しと検証を行い，DuckDB を持たない． |
| `crates/runvault-cli` | `runvault` バイナリ．`query` の背後のインデックスのために DuckDB を同梱する． |

この分割は意図的である．20 数個の再現リポジトリが run を記録するためにライブラリへ依存しており，そのためにデータベースをコンパイルさせるのは，集約側だけが使う機能の代金を全員に払わせることになる．

ライブラリには `schema-gen` feature もある．Rust の型から JSON Schema を出力し，テストが `schema/v1/*.json` と比較できるようにするものである．既定では無効で，スキーマ整合テストを走らせるときだけ必要になる．

## 既知の制約

`crates/runvault` は `schema/v1/vocabulary.toml` を自分のディレクトリの外から `include_str!` で読むため，現状のままでは crates.io に publish できない．これが問題になるのは publish するときだけで，publish の予定はまだない．
