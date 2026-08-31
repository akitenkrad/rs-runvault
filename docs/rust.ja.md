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
