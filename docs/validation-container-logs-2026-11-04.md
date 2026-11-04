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
- 実行Cは原因調査のため、試行中のコンテナのログを `docker logs -f` で別に写した（判定には使っていない）。前提条件の結果がBとDと違うため、B・Dと混ぜて扱わない。`new-soap` の停止は操作の待機の問題で、今回の変更とは関係しない。

### 試験値が出た出力先と行

実行Cで写したログでは、試験値はOpenEMRのコンテナの**標準出力**（Apacheのアクセスログ）の1行にあった。標準エラーとDBのコンテナには無かった。

```
"GET /interface/new/new_search_popup.php?mf_fname=Synthetic&mf_lname=EEL-TEST-…&mf_DOB=2000-01-01&close HTTP/1.1" 200
```

患者登録の重複確認で、氏名（試験値は姓に入る）と生年月日がGETの問い合わせ文字列に入り、アクセスログに残った。実行B・Dでは記録用の末尾（8,192文字）にこの行は入っておらず、変更前の「最後の80行だけを見る」判定では見落として通過していた。

これは、この同梱試験の条件（標準出力・標準エラーに試験値が出ない）を満たさないという観測であり、OpenEMR一般の設定や運用での挙動を示すものではない。アクセスログの設定、ログの保存先・保存期間、ログを見られる人の範囲は確認していない。

## 未確認の範囲

- コンテナ内のファイル（Apacheのログファイルなど）、ログの保存方式の設定で消えた古い行、ログ以外の出力先。
- 実行したイメージと参照したGitコミットのコードの一致。
- 操作の待機の不安定さ（実行Cの `new-soap`）の原因。
- 実AI・Difyとの通しの動作。
