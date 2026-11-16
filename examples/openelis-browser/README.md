# OpenELIS Global 2 の患者登録を試験する

`register-recipe.json` は、架空の患者を登録し、検索結果の表示、保存の応答、DBの行を照合するブラウザ試験。OpenELIS Global 2 の配布用の構成（`DIGI-UW/openelis-docker` のリリース 3.3.0、コミット `6fb4a9e9223f9e04cafbb916d6d5260801ba819e`）の `docker-compose.yml` を参照し、5つのイメージ（DB、証明書、画面、アプリ、入口）をダイジェストで固定する。このリポジトリに、OpenELISのコードとイメージは含めない。

OpenEMR、OpenMRSとは作りの違う対象（PostgreSQL、`public` 以外のスキーマ、HTTPSの入口、ホストから差し込む設定ファイル）を、基盤に対象ごとの処理を足さずに通せるかを確かめるための例である。配布物の軽い構成（FHIRサーバーと分析装置の中継なし）にあたるコンテナだけを起動する。

```sh
npm ci --ignore-scripts
npx playwright install --with-deps chromium
cargo build --locked
umask 077
printf '%s' '{"username":"admin","password":"adminADMIN!"}' > /tmp/eel-openelis-secrets.json
EEL_BROWSER_SECRETS_FILE=/tmp/eel-openelis-secrets.json node tools/recipe-live.mjs examples/openelis-browser/register-recipe.json openelis-register --track-only
```

Dockerのエンジンが必要。試行ごとに新しいDBとコンテナを作り、終了後に消す。既存のOpenELISや実患者のデータには接続しない。ログインの値は、配布物の構成に書かれている既定の管理者で、公開されている。試行の中のコンテナだけで使い、実環境には流用しない。結果は `evaluations/openelis-register/summary.json` に残る。

## 配布物の構成との対応

レシピは、配布物の `docker-compose.yml` と軽い構成（`docker-compose.light.yml`）を、基盤のコンテナの設定に書き写したものである。配布物の設定ファイルは書き換えず、固定したコミットからコンテナの中へ置く。

| 配布物の構成 | レシピでの書き方 |
| --- | --- |
| アプリは、ホストから差し込む `configs/properties/common.properties` を `/run/secrets/common.properties` で読む | `files` で、同じファイルを同じ場所へ置く |
| 入口は、ホストから差し込む `configs/nginx/nginx.conf` をひな形にして設定を作る | `files` で同じファイルを置き、配布物と同じ起動コマンドを書く |
| サービスは `db.openelis.org`、`oe.openelis.org`、`frontend.openelis.org` の名前で互いを呼ぶ | コンテナの名前は `db`、`oe`、`frontend` にし、`aliases` に配布物の名前を書く |
| `certs` は `configs/certs/ensure-stores.sh` で証明書を用意して終了し、アプリと入口はその終了を待つ | `files` でスクリプトを置き、`ready` を `completed` にする。アプリと入口の `start_after` に `certs` を書く |
| 入口は、アプリより先に起動すると終了し、再起動の設定で立ち直る | 入口の `start_after` に `oe` を書く。再起動はしない |

配布物と違うのは、次の点である。

- 起動しないサービスがある。FHIRサーバーと分析装置の中継（軽い構成で外れるもの）、コンテナを自動で再起動する `autoheal`。
- DBのデータとログは、ホストのディレクトリではなく、試行ごとに作って消すボリュームに置く。
- 配布物が差し込む任意の設定（翻訳、メニュー、検査項目の一覧、`SystemConfiguration.properties` など）は置いていない。置かなくてもアプリは起動し、患者登録は通った。
- アプリの固定のIPアドレスは指定していない。

## 画面の操作と判定条件

画面の選択は、起動した 3.3.0 の画面を調べて定義した。患者登録では、National ID に試行ごとの架空の値を入れ、姓と名に固定の文字、性別、推定の年齢を入れて保存する。姓の欄は、記号と数字を含む値を入れても空のまま送られたので、試験値を入れる欄にしていない。保存の後、患者の検索の画面で、同じ値をPatient Idに入れて検索し、結果の表を読む。別の版では、画面の選択が合わなくなることがある。

| 条件 | 調べる根拠 |
| --- | --- |
| 患者の表示 | 検索結果の表に、試行ごとの架空の値が表示されたか |
| 患者登録の応答 | 保存の操作で送った `POST /api/OpenELIS-Global/rest/PatientManagement` の応答がHTTP 200か |
| 患者の保存 | `clinlims` スキーマの `patient.national_id` が試験値に一致する行をDBから読めるか |

扱うのは患者登録の1操作だけである。検体の受付、検査結果の入力、権限は含めない。

## 実行した結果

4コアの作業環境で実行した。基盤は、ファイルを置く指定・別名・終了を待つコンテナを足した版である。作業環境のカーネルにIPv6がないので、下の「IPv6を持たないホストで動かす場合」の置き換えを使った。

| 実行 | 結果 |
| --- | --- |
| `--track-only` で1回 | 判定は「満たした」。準備4手順と操作13手順がすべて完了し、遮断した要求は0件。判定条件3件はすべて期待どおり。全体で148秒 |
| 起動 | DB 1.4秒、`certs` は1.0秒で終了コード0、画面と入口は1秒未満、アプリ 99.4秒。入口はアプリの後に起動した |
| 置いたファイル | `ensure-stores.sh`（2557バイト）、`common.properties`（5653バイト）、`nginx.conf`（2467バイト）。出所・置き場所・SHA-256が実行記録に残った |
| 値の追跡 | 文字列型の列1622件（表361件）を約3.1秒で調べ終え、打ち切りはなかった。試験値は `clinlims.patient.national_id` の1行、保存の要求（`POST /api/OpenELIS-Global/rest/PatientManagement`、応答200）、検索の要求（`GET /api/OpenELIS-Global/rest/patient-search-results`、応答200）、画面の2か所、入口のコンテナのログに現れた |
| 候補（`case draft`） | 読み出し先 `clinlims` スキーマの `patient.national_id`、応答の観測先2件、判定条件（DBの行1、HTTPの状態2、コンテナごとのログに値が出ないこと） |
| 後始末 | コンテナ、ネットワーク、ボリュームの残りは0 |

追跡が見つけた読み出し先と保存の応答の観測先は、レシピに手で書いたものと同じだった。表と列の名前は、追跡には渡していない。

入口のコンテナのログに試験値が出たのは、検索の要求の問い合わせ文字列がnginxのアクセスログに残るためである。候補の「入口のログに値が出ない」という条件を採用すると、期待と異なる結果になるはずだが、採用したレシピでの再実行は行っていない。

`node tools/recipe-count.mjs examples/openelis-browser/register-recipe.json` で数えた手書きの項目は、セレクタ15、通信のURL 1、DBの読み出し先1、判定条件3の計20件。候補で置き換えられるのは、通信のURL、DBの読み出し先、判定条件のうちDBの行とHTTPの状態の計4件である。

ここに至るまでの基盤では、次の結果だった。

| 基盤 | 結果 |
| --- | --- |
| スキーマの指定と起動の順番の指定がない版。役割で順番を合わせたレシピ | 17手順は完了。DBの行は読み出しが失敗して「観測できていない」。判定は「判定できない」 |
| 同じ版。役割を実態どおりに書いたレシピ | 入口が `host not found in upstream "oe"` で準備の前に終了。判定は「判定できない」 |
| スキーマの指定と起動の順番の指定を足した版。設定を環境変数に入れ、名前を書き直し、`certs` を終了しないコマンドに置き換えたレシピ | 判定は「満たした」。全体で152秒 |

## IPv6を持たないホストで動かす場合

配布物の `nginx.conf` は、IPv6でも待ち受ける（`listen [::]:443 ssl`）。カーネルにIPv6がないホストでは、入口がこの行で起動に失敗する。レシピは配布物のファイルのままにしてある。そのホストで試す場合は、`--command` で、その実行に限って、置いたひな形からその行を外す起動コマンドに置き換える。

```sh
EEL_BROWSER_SECRETS_FILE=/tmp/eel-openelis-secrets.json node tools/recipe-live.mjs examples/openelis-browser/register-recipe.json openelis-register --track-only \
  '--command=proxy=["/bin/sh","-c","sed -i '"'"'/\\[::\\]/d'"'"' /etc/nginx/nginx.conf.template && envsubst '"'"'$OE_NGINX_CERT $OE_NGINX_KEY $OE_HTTPS_PORT'"'"' < /etc/nginx/nginx.conf.template > /etc/nginx/nginx.conf && exec nginx -g '"'"'daemon off;'"'"'"]'
```

IPv6のあるホストで、置き換えなしの実行は確かめていない。

## 確かめていないこと

- 故障を入れた条件（DBの停止、切断、読み取り専用）。
- 保存途中の切断（`relay_cut`）。文を記録する中継はPostgreSQLのプロトコルに対応していないので、この対象では使えない。
- 操作の記録（`tools/browser-recorder.mjs`）で、画面の選択を手書きせずに作ること。
- 患者登録のほかの操作と、3.3.0 のほかの版。FHIRサーバーと分析装置の中継を含む構成。
- 追跡の候補を採用したレシピでの再実行。
- CIでの実行。CIでは、レシピが入力の検査を通ることと、手書きの項目数だけを確かめる。
