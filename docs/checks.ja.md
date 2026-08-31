[English](checks.md) | **日本語**

# 検査

JSON Schema が見るのは一度に 1 ファイル，あるいは 1 行である．run が *自分自身と* 矛盾していないかは別の問いであり，それを尋ねるのが `runvault verify` である．

```bash
runvault verify <run>          # ファイルをまたぐ不変条件
runvault verify <run> --deep   # …に加えて，コストが run の大きさに比例するもの
```

## shallow: ファイルをまたぐ不変条件

どの run に対しても，いつでも尋ねられる程度に安い．

| 検査対象 | 内容 |
| --- | --- |
| slug ↔ ハッシュ | ディレクトリ名，`run_slug`，slug が含む 2 つのハッシュ接頭辞がすべて一致する |
| `config.json` | エンベロープがこの run のものであり，制御ブロックが解決できる |
| `status.json` | 存在し，この `run_uid` について書かれている |
| `metrics.csv` | 全行がこの run の `run_uid` を持ち，キーが重複しない |
| `reference.csv` | 同上．加えて全 `target_id` が宣言済みの `research.targets[]` のいずれかである |
| `manifest.csv` | 同上 |
| `events.jsonl` | 同上を 1 行ずつ |
| `research` | 再現対象が同定されており，名乗る id が実際に持つ id と一致する |
| `data` | 各エントリが `hash` / `dataset_id` / `uri` のいずれかを持ち，`(role, name)` が一意である |
| `lineage` | 自己参照が無く，循環が無く，`resumed_from` が許された対象を指している |

循環検査は resume / derive の連鎖だけでなく lineage の 3 辺すべてを辿る．互いを sweep 親と名乗る 2 つの run は，resume のループと同じくらい辿れないからである．run は別の場所にある run を参照することもあり，それらはここからは検査できない —— 検査できないことは誤りであることとは別である．

## deep: run の大きさだけコストが掛かるもの

`--deep` は上記をすべて実行したうえで：

- **3 つのハッシュを再計算する**．それらを要約していると称するファイルから計算し直す．これが，後から `parameters` を書き換えた run が古い `config_hash` を名乗り続けるのを止める．再計算した値は，記録された値ではなく次のハッシュへ送り込まれる —— たった今それらが等しいと証明されたのだから，再計算値を使う方が連鎖として誠実である．
- **`artifacts/` を再ハッシュする**．これが，生成ファイルが記録の外で生きるのを止める．
- **`events.jsonl` を最後まで歩く**．

コストは run の大きさに比例するので，毎回の実行が終了時に行うものにはしていない．`runvault sync` はコピー前にこれを走らせ，通らない run は送らない．集約層が壊れた run を受け入れることがないようにするためである．

## リポジトリ自身の検査を走らせる

```bash
cargo test --all-features
uv run --with jsonschema --with rfc3339-validator python tools/test_schemas.py
uv run --with blake3 python tools/gen_testvectors.py   # 1 バイトも変わってはならない
```

```bash
cd python && uv run --group dev pytest -q
```

`tools/gen_testvectors.py` は正規化とハッシュ規則の第 2 実装である．そこから出るベクタこそがテストの照合先であり，2 つが食い違ったならどちらかが誤っている．再生成は解決ではない．[スキーマ](schemas.ja.md) を参照．

## テストデータ

`crates/runvault/tests/fixtures/legacy/` 配下の run ディレクトリは，著者自身による Schelling (1971) の再現と，小さな意見ダイナミクスモデルの出力を，各数行に切り詰めたものである．テストデータであって，結果ではない．
