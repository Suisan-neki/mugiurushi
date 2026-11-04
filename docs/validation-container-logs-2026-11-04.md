# コンテナのログの判定と、実OpenEMRでのログの試験（2026年11月4日）

対象はPR #205（`claude/container-log-complete`）。本書は、合成データの試験と実OpenEMRでの実行を分けて記録する。実OpenEMRの結果は、OpenEMRや期待条件を変えずにそのまま残す。

## 変更の要点

- コンテナのログは最後の80行・8,192文字だけを、標準出力だけから読んでいた。標準出力と標準エラーをそれぞれ4MiBまで読んで判定する。最後まで読めなければ「含まない」を「観測できていない」にする。
- ブラウザ試験で操作を一つも行っていない（準備で止まった）、かつ試験値を検出していない場合は、ログに無いことを「観測できていない」にする。試験値を検出した場合は、操作の有無にかかわらず「含まない」は失敗のまま（検出を隠さない）。
- 同梱試験「OpenEMR：SOAPの保存でログに試験値が出ないか」。保存できたこと（`soap-subjective-row`、DBの行）を前提に、OpenEMR（`soap-log-app`）とDB（`soap-log-db`）のコンテナの標準出力・標準エラーに試験値が出ないことを判定する。コンテナ内のファイルに書かれたログは見ない。

## 合成データの試験（CIと作業環境）

- `tests/trial.rs` の `container_log_checks_use_the_whole_log_and_never_pass_absence_on_a_partial_read`：全文にあり記録用の末尾にない値は「含む」。最後まで読めていない場合の「含まない」は観測できていない。操作していない・未検出は観測できていない。準備中に検出した値は「含まない」が失敗のまま。記録から読み直しても通過しない。操作していない場合の確認は、修正前のコードで失敗することを確かめた。
- `src/container.rs` の単体試験：上限を超えたログは最後まで読めていない扱い。末尾の抜粋は80行まで。
- `tests/containers.rs` の `container_log_checks_include_the_targets_standard_error`（`--ignored`、Docker）：標準エラーに書いた値を検出する。作業環境で実行して成功。
- `tools/browser-handoff-smoke.mjs`（合成、実行直前で停止）：同梱の3試験を表示し、ログの試験の条件が画面に出る。

## 実OpenEMRでの実行

### 実行環境

- Docker 29.9.2。イメージの取得は、Network secret（auth.docker.io の `/token` へのBasic認証）と、秘密でない固定文字列による `docker login` で行った。PATの値は取り出していない。
- 試験用ブラウザは、レシピが固定する Chromium 151.0.7922.34（Playwright 1.62.1、revision 1234）。この作業環境に入っていたのは別の版（153.0.8010.12）で、レシピの版の検査により実行できなかった。Playwrightの配布元はネットワーク方針で遮断されていたため、同じ版の Chrome for Testing を Google の公開配布元から取得した（ZIPのSHA-256 `ae8736ac28bc69278551500f219fc749575648263c43ec5990749eff43b9fcf8`）。
- 実行器は Chromium のOSサンドボックスを必須とし、`--no-sandbox` を拒否する。root ではサンドボックス付きで起動できないため、root でない試験用ユーザーで実行した。条件は緩めていない。
- 実行したイメージ：`openemr/openemr:latest@sha256:f0bfbc17fae13503ab64bbd271f74d80dd4a0786f5a8d0683841aeef4e159700`、`mariadb:12.3.3@sha256:2bdff1534a7e569fecaf4b10b66ad0d410806e382d9db29ba305448addda97c4`（取得後のダイジェストは指定と一致）。参照したコードは `openemr/openemr` の `d7ecc612e166c6f9f3adc9f50bc9f8cc68cac701`（`docker/production/docker-compose.yml`）。**実行したイメージの中身と参照したGitコミットのコードが同一かは確認していない。**
- 外部通信は遮断（`egress: blocked`、遮断を確認済み）。試行後に残ったコンテナ・ネットワーク・ボリュームはない。

### 再現の手順

```sh
cargo build --locked
printf '%s' '{"username":"eel-operator","password":"eel-pass-trial"}' > /tmp/openemr-secrets.json
EEL_BROWSER_SECRETS_FILE=/tmp/openemr-secrets.json \
EEL_BROWSER_EXECUTABLE=<Chromium 151.0.7922.34 の実行ファイル> \
node tools/openemr-logs-live.mjs
```

結果は `evaluations/live-openemr-logs/`（`summary.json`、`evidence.json`、`recipe.json`、事例の保存先）に残る。合否は麦漆の判定をそのまま保存し、スクリプトでは決めない。試験用アカウントは公開された架空の値で、実環境に流用しない。

### 結果（実行ごとに分けて記載）

| 実行 | コード | 前提（SOAPの保存） | OpenEMRのログ | DBのログ | ログを最後まで読めたか | 全体の判定 |
| --- | --- | --- | --- | --- | --- | --- |
| A | `0612e38`（修正前）、Chromium 153 | 観測できていない（ブラウザの版が異なり準備で停止。保存の操作なし） | 期待どおり（**誤り。操作していないため**） | 期待どおり（同上） | 両方読めた | 判定できない |
| B | `0612e38`、Chromium 151 | 期待どおり（DBに1行） | **期待と異なる（試験値あり）** | 期待どおり | 両方読めた（OpenEMR 34,474 B、DB 10,850 B） | 満たさない |
| C（調査） | `0612e38`、Chromium 151、ログを別経路で写した | 期待と異なる（手順 `new-soap` が15秒の待機上限で止まり保存せず。DBは読めて0行） | 期待と異なる（保存前の患者登録で出力済み） | 期待どおり | 両方読めた | 満たさない |
| D（最終） | `01e7b3a` | 期待どおり（DBに1行） | **期待と異なる（試験値あり）** | 期待どおり | 両方読めた（OpenEMR 34,474 B、DB 10,545 B） | 満たさない |

- 実行Aで、操作していないのにログの条件が期待どおりになる不具合を見つけ、`050accc` で直した。
- 実行Cは原因調査のため、試行中のコンテナのログを `docker logs -f` で別に写した（判定には使っていない）。前提条件の結果がBとDと違うため、B・Dと混ぜて扱わない。`new-soap` が待機上限で止まった原因は特定していない（同じ手順は実行B・Dでは完了した）。

| 実行 | run_id | evidence_id | 証跡（実行した作業ツリーからの相対パス。リポジトリには含めていない） |
| --- | --- | --- | --- |
| A | `run-1793773073246-5072-0` | — | 残っていない（同じ出力先を使った実行Bで上書きした。上の値は上書き前に取り出したもの） |
| B | `run-1793773246013-9118-0` | `evidence-1` | `out-205/summary.json`、`out-205/evidence.json`（作業用の出力先） |
| C | `run-1793773367144-14017-0` | `evidence-1` | `out-205b/summary.json`、`out-205b/evidence.json`、別経路で写したログ（作業用） |
| D | `run-1793773991899-6857-0` | `evidence-1` | `evaluations/live-openemr-logs/summary.json`、`evaluations/live-openemr-logs/evidence.json`、`evaluations/live-openemr-logs/case/`（`01e7b3a` のツリー） |

#203と#205を含む一時ブランチ（`a31cd8915088328192a0a496f5c2766c8bd9863d`、ツリー `d87e726b5e6c98491ecc1e25a73be322046ae999`）での `tools/browser-handoff-smoke.mjs --live` は run `run-1793774152885-15039-0`。証跡は `test-results/browser-handoff-live.json` と画面写真（`test-results/browser-handoff-*.png`）。この試験は終了時に一時ディレクトリを削除するため、事例の保存先と evidence_id は残っていない。

### 試験値が出た出力先と行

実行Cで写したログでは、試験値はOpenEMRのコンテナの**標準出力**（Apacheのアクセスログ）の1行にあった。標準エラーとDBのコンテナには無かった。

```
"GET /interface/new/new_search_popup.php?mf_fname=Synthetic&mf_lname=EEL-TEST-…&mf_DOB=2000-01-01&close HTTP/1.1" 200
```

患者登録の重複確認で、氏名（試験値は姓に入る）と生年月日がGETの問い合わせ文字列に入り、アクセスログに残った。実行B・Dでは、判定に使ったログの全文には試験値があり、記録用の抜粋（標準出力と標準エラーを続けた末尾8,192文字）には無かった。変更前の実装（`docker logs --tail 80` の標準出力の先頭8,192文字）を同じログに対して実行してはいないため、変更前の判定がこの行を見落としたかどうかは確かめていない。

これは、この同梱試験の条件（標準出力・標準エラーに試験値が出ない）を満たさないという観測であり、OpenEMR一般の設定や運用での挙動を示すものではない。アクセスログの設定、ログの保存先・保存期間、ログを見られる人の範囲は確認していない。

## 未確認の範囲

- コンテナ内のファイル（Apacheのログファイルなど）、ログの保存方式の設定で消えた古い行、ログ以外の出力先。
- 実行したイメージと参照したGitコミットのコードの一致。
- 実行Cで `new-soap` が待機上限で止まった原因（今回の変更との関係も含め未特定）。
- 変更前の実装を同じログに対して実行した場合の結果。
- 実AI・Difyとの通しの動作。
