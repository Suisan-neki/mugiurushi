# 架空の診療所の問診（デモ用）

mugiurushi の「ガイドラインの照合から試験・修正・再試験まで」の流れ（`/flow`）を、試験を自動で実行して見せるための架空のアプリ。実在の診療所・患者・職員のデータは扱わない。

- 患者がコマンドで問診に答え（`answer`）、職員が回答を読む（`show`）。回答は患者ごとのファイル（`answers/患者ID.tsv`）に保存する。
- 最初の版（`main`）は、受付の職員も問診の回答を読める。
- 修正版（`fix/answer-access`）は、回答の内容を読めるのを医師だけにする。受付の職員には、回答があるかどうかだけを示す。
- 標準ライブラリだけで動く。依存パッケージはない。
- `tools/monshin-demo.mjs` が、この2つのブランチを持つ架空のリポジトリ `example/monshin` を手元に作り、mugiurushi を起動する。

```bash
python3 monshin.py --dir . --user P001 answer P001 "頭痛が3日続いている"
python3 monshin.py --dir . --user doctor_d show P001
```
