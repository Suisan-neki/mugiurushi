# 開発・実行の手引き

この文書は開発者向けの起動手順、実行上の制約、コード構成と確認方法をまとめたものです。プロジェクトの背景と概要は[README](../README.md)を参照してください。

## 起動

対応OSはLinux・macOSです。Unix固有のプロセス・ファイル操作を使うため、Windowsではビルドできません。Rust 1.99.0を使います。開発者・運営者が最初にAI接続を設定します。

```bash
git clone https://github.com/Suisan-neki/mugiurushi.git
cd mugiurushi
cargo run -- ai init
# 作成された data/app/ai.json をエディタで開き、api_key と model を記入する
cargo run -- ai test
cargo run
```

表示されたlocalhostのURLを開きます。既定の保存先は`data/app`、ポートは8765です。設定ファイルはGitの管理対象外で、既存の設定は上書きしません。利用者向け画面にはキーの入力欄を設けません。

ガイドラインとの照合・試験の案などを使う場合は、運営者が起動時に、AIエージェント（[mugiurushi-ai](https://github.com/Suisan-neki/mugiurushi-ai)）の接続先`EEL_AGENT_URL`と、共有トークンを書いたファイル`EEL_AGENT_TOKEN_FILE`を指定します（[構成図とAI接続](repository-architecture.md)）。未設定・失敗のときは、AIを使う欄を出さないか失敗を示し、確認項目・試験・判定はAIなしで使えます。GitHubのAPIの利用上限を広げる場合は、トークンを書いたファイルを`EEL_GITHUB_TOKEN_FILE`に指定します。読み取りは公開リポジトリだけで、非公開のリポジトリは拒否します。

```bash
cargo run -- app data/app --port 8765 --execution-mode local
cargo run -- --help
```

画面から選ぶブラウザ試験の一覧を使う場合は、`EEL_RECIPE_DIR=examples`のように一覧のディレクトリを指定して起動します。指定しない場合、一覧は空です。形式は[ブラウザ操作の試験](browser-trials.md#画面から選ぶ試験の一覧)を参照してください。

`local`で起動するNode.js・Pythonとブラウザ操作器はホスト上で動き、OSによる隔離を行いません。コンテナの対象は、既定で外部へ出られない内部ネットワークで動かし、権限を絞ります（下記）。ホスト側の操作器まで隔離する方式ではありません。Dockerの操作権限は強い権限を持つため、専用の試験環境で使います。信頼できる対象を使い、実行内容を確認してから試験を開始します。Linuxの`bubblewrap`を指定した場合、隔離を起動できなくてもlocalへ切り替えません。

コンテナの設定の`egress`は、既定の`blocked`で試験用ネットワークを`--internal`にし、対象から外部へ通信できなくします。ホストの127.0.0.1と、対象どうしの接続は、基盤が対象ごとに起動する中継コンテナ（固定イメージのnginx）を通ります。`egress: "allowed"`を明示した場合だけ外部へ通信でき、実行記録に`allowed`と残ります。対象のコンテナは`--cap-drop ALL`に最小限の権限だけ戻し、`no-new-privileges`とプロセス数の上限を付けます。中継は接続ごとに接続元・接続先の対象、成否、バイト数を記録し、実行記録の`containers.traffic`へ集計します。`failed`は中継の状態が200以外だった接続で、クライアントの切断や中継停止も含み、アプリの処理の失敗件数ではありません。閉じなかった接続は`unfinished`の件数だけで、成否とバイト数は分かりません。通信の中身（SQLや本文）、中継を通らない通信、`egress: "allowed"`の試行は記録しません。試験案の`recovery`（`ready_timeout_ms`）を指定すると、DBの停止・切断・読み取り専用の故障を解除し、各コンテナの準備と、故障中に行ったDB行・HTTPの読み出しを復旧後にもう一度行って、実行記録の`trial.recovery`へ残します。故障中に読めなかった行は「復旧後に読めた」と記録し、「行がなかった」とは扱いません。採用の判定には使いません。故障中に失われた操作の再実行と、実運用の復旧手順の妥当性は確認しません。この遮断は対象のコンテナからの通信が対象で、基盤・ブラウザ操作器・中継コンテナのホスト側には及びません。

コンテナは1つずつ起動し、準備ができてから次を起動します。順番は、役割の順（DB、その他、アプリ）と一覧の順です。`start_after`にコンテナの名前を書くと、そのコンテナの準備ができた後に起動します。起動のときに転送先の名前を解決する入口のnginxのように、ほかのコンテナが先にないと起動に失敗する対象に使います。順番を決められない指定（互いを待つ指定、存在しない名前）は、設定の検査で拒否します。`files`に`from`（リポジトリの最上位からの相対パス）と`to`（コンテナの中の絶対パス）を書くと、固定したコミットのファイルを、起動の前にコンテナの中へ置きます。置けるのは1コンテナ16件まで、1件1MiBまでの通常のファイルです。親のディレクトリがなければ作ります。所有者はroot、権限は0644（実行できるファイルは0755）、時刻は0です。置いたファイルの出所・置き場所・大きさ・SHA-256は、実行記録の`containers.services[].files`に残します。ディレクトリごとの指定と、レシピに内容を直接書く指定はありません。`aliases`にホスト名を書くと、試験用ネットワークの中で、その名前でもコンテナに届きます。ドットを含む名前を書けます。ほかのコンテナの名前や別名と重なる指定は拒否します。`ready`を`"completed"`にしたコンテナは、終了コード0で終わったときに準備できたとみなします。証明書の用意のように、1回動いて終わるコンテナに使います。このコンテナには待ち受けポート（`expose`）を書かず、中継も置きません。0以外の終了コードと時間切れは、準備できなかったと記録し、後のコンテナを起動しません。

コンテナ1つの準備を待つ上限（`ready_timeout_ms`）は、最大1800000ミリ秒（30分）です。初回の起動でDBのスキーマを作る対象は、数分では準備できないことがあります。復旧後の待機上限（`trial.recovery.ready_timeout_ms`）は最大300000ミリ秒です。実行記録に残すログの抜粋は、標準出力と標準エラーを合わせて、対象が書いた順の最後の80行です。試行の後、基盤はコンテナ・ネットワーク・ボリュームを消します。イメージが`VOLUME`で宣言した場所にできる、設定に書いていないボリュームも消し、その名前を`containers.image_volumes`に残します。消えずに残った資源は`containers.leftovers`に記録し、試行の診断に出します。

Cargoパッケージ名とバイナリ名は`mugiurushi`です。旧名称はexecution-evidence-lab、kintsugiです。

証拠不足の判定はUnknownとして残し、画面・API・確認資料で検証未通過と示します。確認事項が空の場合や未確認・未達の条件がある場合は、「採用する」という判断の記録をサーバー側で拒否します。記録した全条件の通過も、実運用の許可や医療安全性の保証を意味しません。

## 構成

| 場所 | 役割 |
| --- | --- |
| `src/main.rs` | CLIの呼び出しと終了時のエラー表示 |
| `src/cli/` | アプリ・AI設定・ケース管理・固定環境のCLI |
| `src/review.rs` | 現在の画面と操作トークン付きAPI |
| `src/reader/`・`src/understanding.rs` | 固定版の読取・静的解析・読取記録 |
| `src/agent.rs` | AIエージェント（mugiurushi-ai）の呼び出し、送る前の確認、応答の検査 |
| `src/system_diagram/` | 構成図、部分ごとの会話、期待と結果への対応づけ |
| `src/execution.rs`・`src/trial.rs` | 固定版の実行、故障条件、観測と判定 |
| `src/shutdown.rs` | 終了の合図（SIGTERM・SIGINT）を受けたときの、子のプロセスとコンテナの回収 |
| `src/recording.rs`・`src/run_record.rs` | 出来事・試行情報・完了記録の保存 |
| `src/user_trial.rs`・`ui/web-trial.*` | 過去の試作のWeb試用記録（保存済みデータを継続して扱う） |
| `src/workspace.rs`・`src/comparison.rs` | 条件、根拠、人の判断、履歴と試行比較 |
| `src/backup.rs`・`src/review_report.rs` | 全版のバックアップ・復元と確認資料 |
| `ui/app.*`・`ui/repository-diagram.js` | 構成の調査と検証課題の画面 |
| `ui/workbench.*` | コード確認、検証設定、実行、結果と比較 |
| `ui/flow.*`・`ui/flow-record.js` | ガイドラインとの照合・ヒアリング・試験の案の画面（`/flow`） |

ケース操作とAPI起動は`--help`を参照してください。`case init`は条件や回答を含まない空のケースを作ります。過去の試作の廃止と保存済みデータの扱いは[整理と確認範囲](current-workflow-cleanup.md)に記録しています。

## 確認

```bash
cargo fmt --check
cargo check --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked
python3 tools/check-yomiyasu.py
npm ci --ignore-scripts
npm run typecheck
npm run lint
npm run test:api
npx playwright install --with-deps chromium
npm run test:ui
npm run test:reader-native
npm run test:flow
npm run test:flow-record
npm run test:flow-rescan
```

PRのCI（`.github/workflows/rust.yml`）は、このほかにも固定フィクスチャの試験を実行します。必須の一覧はワークフローを正とします。`test:ui`は構成図と対話を実Rustサーバー・合成AIで操作します。`test:flow`系は照合の画面（`/flow`）を合成AIで操作します。

`test:app`は公開リポジトリsjl/tを使って、検証・故障・改修・再試験・比較を操作します。公開GitHubに依存するため、PRでは実行せず、手動・週次のワークフロー（`.github/workflows/live-integration.yml`）で実行します。合成AIでの成功は実モデルの品質や利用者の理解を証明しません。

ブラウザを使わずに対話のAPIを確認する場合は、`EEL_DIALOGUE_API_ONLY=1 npm run test:dialogue`を使います。現在の実行経路については`tools/queue-demo.mjs`と`tools/pinned-demo.mjs`でも検証できます。ここでの番号表示は実際にコードを起動する参照アプリであり、廃止した名前・年齢デモとは別です。

## 操作と研究の記録

- [現在の対象・目的・評価方針](developer-verification.md)

- [構成図とAI接続](repository-architecture.md)
- [対話と検証への接続](diagram-dialogue.md)
- [対話の追加読取とブラウザ試験](dialogue-browser-handoff.md)
- [図の読みやすさとyomiyasu](readable-diagram-and-writing.md)
- [固定版実行と制約](pinned-execution.md)
- [医療OSSの固定環境](pinned-environments.md)
- [今回の整理と確認範囲](current-workflow-cleanup.md)

既存の`/user-trial`と保存済みの試用記録は引き続き読めますが、技術的な検証の主導線からは外しています。

`docs/`には過去の試作・研究経緯も残しています。廃止したコマンドや画面を含む過去の記述は、現在の操作手順ではありません。

## 検証の実行

PRではRustの検査と、合成データ・固定フィクスチャによる試験を実行します。公開GitHub、Docker、外部OSSに依存する試験は手動・週次実行のワークフローに分けています。OpenEMR・OHIF・MONAIの試験は検証例であり、PRごとの必須条件にはしません。

実行を途中で止める場合は、実行器へSIGTERMまたはSIGINT（端末のCtrl+C）を送ります。実行器は、その試行で起動した子のプロセスと、コンテナ・ネットワーク・ボリュームを回収し、行ったことと残った資源を標準エラーへ書いてから、終了コード143または130で終わります。回収の途中で2回目の合図を受けても回収を続け、3回目の合図で回収を待たずに終わります。SIGKILLでは回収は動きません。

簡易解析は正規表現で宣言や参照を抽出します。テンプレート文字列、複数行の式、マクロ、動的な参照などの意味を正確に解析するものではありません。抽出した行の存在、解析の推定、AIの説明、実行で得た観測を区別します。

画面版の操作トークンはHTMLにも含まれ、別サイトからの操作を防ぐために使います。同じ端末の別プロセスや利用者を認証する仕組みではありません。API専用モードはトークン付きHTMLを配信せず、所有者だけが読めるトークンファイルで操作します。

開発ビルドはUIをチェックアウト内のファイルから配信するため、JS・CSSの変更にRustの再ビルドは不要です。配布時は`cargo build --release --locked`を使い、UIを実行ファイルへ埋め込みます。試行の`source_fingerprint`は起動した実行ファイルのSHA-256です。ソースの署名ではありません。ソースのコミットも記録する場合は、ビルド時に`MUGIURUSHI_SOURCE_REVISION`へ40桁のSHAを設定します。未設定なら`unrecorded`と記録します。

HTTP受付、処理ジョブ、UI資材、事例の永続化はそれぞれ`src/review/`・`src/workspace/storage.rs`へ分けています。UIのHTTP操作と画面部品も別モジュールにしています。`npm run lint`はUI全体、`npm run typecheck`は共通HTTPモジュールを検査します。従来の画面コード全体の型移行は完了していません。
