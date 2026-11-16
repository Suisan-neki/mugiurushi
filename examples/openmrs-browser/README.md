# OpenMRS 3 の患者登録を試験する

`register-recipe.json` は、架空の患者を登録し、登録後の表示、保存の応答、DBの行を照合するブラウザ試験。OpenMRS 3 の参照用の配布物（`openmrs/openmrs-distro-referenceapplication` の固定コミット `299b0cbcd78123cfa38741dbd8161e8b29dd13be`）の `docker-compose.yml` を参照し、記載された4つのイメージ（MariaDB 10.11.7、backend・frontend・gateway の 3.8.0）をダイジェストで固定する。このリポジトリに、OpenMRSのコードとイメージは含めない。

OpenEMRとは作りの違う対象（Javaのバックエンド、Reactの画面、REST API、入口のnginx）を、基盤に対象ごとの処理を足さずに通せるかを確かめるための例である。

```sh
npm ci --ignore-scripts
npx playwright install --with-deps chromium
cargo build --locked
umask 077
printf '%s' '{"username":"admin","password":"Admin123"}' > /tmp/eel-openmrs-secrets.json
EEL_BROWSER_SECRETS_FILE=/tmp/eel-openmrs-secrets.json node tools/recipe-live.mjs examples/openmrs-browser/register-recipe.json openmrs-register --track-only
```

Dockerのエンジンが必要。試行ごとに新しいDBとコンテナを作り、終了後に消す。既存のOpenMRSや実患者のデータには接続しない。ログインの値は、参照用の配布物が初期化で作る既定の管理者で、公開されている。試行の中のコンテナだけで使い、実環境には流用しない。結果は `evaluations/openmrs-register/summary.json` に残る。

`tools/recipe-live.mjs` は、レシピを1つ受け取って事例を作り、基盤の実行器で1回実行する。対象ごとの処理は持たない。`--track-only` は入力した値の追跡を有効にし、追跡した試行から読み出し先と判定条件の候補（`case draft`）も出す。

## 起動にかかる時間

バックエンドは、初回の起動でDBのスキーマを作る。4コアの作業環境では、準備できるまで約13分かかった（下の表）。レシピはバックエンドの `ready_timeout_ms` を1500000（25分）にしている。試行のたびに新しいDBを作るので、この時間は毎回かかる。

サービスは、DB、frontend、gateway、backend の順に1つずつ起動する。バックエンドの準備は `/openmrs/health/started` が200を返すことで判定する。この経路は、起動の途中では503を返す。

## 画面の操作と判定条件

画面の選択は、起動した 3.8.0 の画面を調べて定義した。ログインは、利用者名の入力、次へ進むボタン、パスワードの入力の順に進む。ログイン後の場所の選択では Outpatient Clinic を選ぶ。患者登録では、名に `Taro`、姓に試行ごとの架空の値を入れ、性別を選び、生年月日は不明として推定の年齢を入れる。別の版では、画面の選択が合わなくなることがある。

| 条件 | 調べる根拠 |
| --- | --- |
| 患者の表示 | 登録後の患者の見出し（patient banner）に、試行ごとの架空の値が表示されたか |
| 患者登録の応答 | 保存の操作で送った `POST /openmrs/ws/rest/v1/patient/` の応答がHTTP 201か |
| 患者の保存 | `person_name.family_name` が試験値に一致する行をDBから読めるか |

扱うのは患者登録の1操作だけである。受診、診療記録、権限、同じ患者の重複の確認は含めない。

## 実行した結果

基盤のコミット `3fe608f` で、4コアの作業環境（IPv6なし。下の置き換えを使用）で実行した。

| 実行 | 結果 |
| --- | --- |
| `--track-only` で1回 | 判定は「満たした」。準備7手順と操作8手順がすべて完了し、遮断した要求は0件。判定条件3件はすべて期待どおり。全体で811秒 |
| 起動 | DB 2.4秒、frontend と gateway は1秒未満、backend 776.5秒 |
| 値の追跡 | 文字列型の列992件（表228件）を約1.4秒で調べ終え、打ち切りはなかった。試験値は `openmrs.person_name.family_name` の1行、`POST /openmrs/ws/rest/v1/patient/` の要求（応答201）、画面の1か所に現れた。コンテナのログには現れなかった |
| 候補（`case draft`） | 読み出し先 `openmrs.person_name.family_name`、応答の観測先 `POST /openmrs/ws/rest/v1/patient/`、判定条件6件（DBの行1、HTTPの状態1、4つのコンテナのログに値が出ないこと4）。画面の表示は、読む要素を指定する手順が要るので候補にならなかった |
| 後始末 | コンテナ、ネットワーク、ボリュームの残りは0 |
| バックエンドの上限を120秒にした実行 | 判定は「判定できない」。判定条件3件は「観測できていない」。backend の記録の末尾に、時間切れの時点で実行中だったLiquibaseの変更の行が残った |

追跡が見つけた読み出し先と応答の観測先は、レシピに手で書いたものと同じだった。表と列の名前は、追跡には渡していない。

`node tools/recipe-count.mjs examples/openmrs-browser/register-recipe.json` で数えた手書きの項目は、セレクタ14、通信のURL 1、DBの読み出し先1、判定条件3の計19件。候補で置き換えられるのは、通信のURL、DBの読み出し先、判定条件のうちDBの行とHTTPの状態の計4件である。セレクタ14件と画面の表示の条件1件は、手書きのまま残る。

バックエンドの準備にかかった時間は、上限だけを手元で延ばした実行ファイルでの2回（781秒、791秒）と、この実行（777秒）の3回とも約13分だった。変更前の上限（300秒）のままの基盤では、判定は「判定できない」になった。

## IPv6を持たないホストで動かす場合

gateway のイメージのnginxは、IPv6でも待ち受ける設定（`listen [::]:80`）で起動する。カーネルにIPv6がないホストでは、この行で起動に失敗し、gateway が準備できない。レシピは配布物の起動のしかたのままにしてある。そのホストで試す場合は、`--command` で、その実行に限って起動コマンドを置き換える。

```sh
EEL_BROWSER_SECRETS_FILE=/tmp/eel-openmrs-secrets.json node tools/recipe-live.mjs examples/openmrs-browser/register-recipe.json openmrs-register --track-only \
  '--command=gateway=["sh","-c","sed -i '"'"'/\\[::\\]/d'"'"' /etc/nginx/templates/default.conf.template && exec /docker-entrypoint.sh nginx -g '"'"'daemon off;'"'"'"]'
```

下の結果は、この置き換えを使った実行のものである。IPv6のあるホストで、置き換えなしの実行は確かめていない。

## 確かめていないこと

- 故障を入れた条件（DBの停止、切断、読み取り専用）と、保存途中の切断（`relay_cut`）。
- 操作の記録（`tools/browser-recorder.mjs`）で、画面の選択を手書きせずに作ること。
- 患者登録のほかの操作と、3.8.0 のほかの版。
- 追跡の候補を採用したレシピでの再実行。
- CIでの実行。初回の起動に時間がかかるので、CIには入れていない。CIでは、レシピが入力の検査を通ることと、手書きの項目数だけを確かめる。
