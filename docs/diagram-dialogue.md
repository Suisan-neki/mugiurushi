# 構成図への疑問を検証へ引き継ぐ

利用者は図の箱・接続・全体を選び、不安や疑問をAIと整理する。会話で分かった操作する人・操作・期待する結果、関係する構成要素、コードの根拠、未確認事項、未実施の検証案を同じ画面で確認する。立場の変更は説明の詳しさに使い、構成や結果を変更しない。

利用者が期待する動作を確認・編集して「この内容を検証する」を押すと、疑問と期待・未回答の確認事項を既存の事例へ保存し、検証の設定へ進む。実行には既存の起動・観測設定を使う。会話の送信だけでは、検証事項、観測、合意を追加しない。

保存済みの検証があれば同じ内容を再利用する。期待を変更した場合は同じ検証事項の版を更新し、過去の根拠を残す。検証画面から元の図の版と選択箇所へ戻り、同じ対話を再開できる。以後の相談では、その対話から保存した検証事項の結果をサーバーが渡す。別の対話の結果を混ぜない。

## サーバーが保持する内容

対話の識別子はリポジトリ・コミット・図の版・選択箇所から決める。各往復に利用者の発言、立場、返答、構造化した整理案、参照行の照合結果、そのとき渡した試験結果を保存する。読取記録の `.system-diagrams/<repository>/<commit>/dialogues/` に置き、保存の完了前のファイルを現在の履歴として読まない。

異なる図の版や選択箇所は別の対話になる。立場を切り替えたり、ブラウザを再読み込みしたりしても同じ対話を読める。期待を事例へ取り込んだ際の対話ID・往復番号を出所として残す。事例のバックアップには疑問・期待・未確認事項と出所を含むが、別ディレクトリにある読取記録・図・会話ファイルの一括バックアップは未対応。

AIの返した整理案は `ai_draft`。記録には `concern`、`actor`、`operation`、`expectation`、`related_object_ids`、`claims`、`unknowns`、`test_plan`、`reported_observations`、`improvement_ideas` を持つ。基盤の判定や人の合意の欄をモデルに書かせない。参照行の存在・今回渡した範囲・図のID・リクエストID・文字数を検査する。根拠の意味の正しさまで証明する検査ではない。

対話で提示した検証案は、実行可能なRecipeへ自動変換されない。検証内容を記録すると、検証案は確認項目の「確かめたい部分」の下書きになり、人が観測の種類と試験の判定条件を選んで登録する（[対話からブラウザ試験への引き継ぎ](dialogue-browser-handoff.md)）。相談内容と試験条件の意味が一致するかは文章から自動で検査しない。画面に表示する試験の判定は選択した条件についての結果であり、`verification_passed`と採用判断の可否は、試験の判定に加えて、人が登録した部分を麦漆が判定条件IDと観測方法で照合してすべて観測した場合だけ真になる。登録していない部分、基盤が観測できない部分は未確認のまま残り、相談全体を網羅した証明にはならない。

`reported_observations` は利用者の報告。`results` は事例から読んだ結果であり、各記録の観測方法・適用範囲を保持する。コードの記述、画面の成功表示、保存先の状態を同じ事実にしない。ログの欠落だけで失敗と判定しない。

## Difyと既存AI接続

運営者が `data/app/dify.json` を設定した場合、対話にDify Chatflowを使う。図の生成には引き続き既存の `ai.json` を使う。Difyの設定がなければ、既存のOpenAI互換接続で同じ整理案と返答を生成する。Difyが設定済みの場合の失敗は利用者へ返し、別の接続先へ自動で切り替えない。

運営者は `dify.example.json` を保存先の `dify.json` へコピーし、権限を0600にしてDifyアプリ用のAPIキーを記入する。これはモデル提供元のOpenAI APIキーとは別。設定は起動時に読み、変更後は再起動する。利用者の画面やHTTP APIから接続設定を作成・変更する処理は設けない。

作成したDifyアプリは[システムの疑問を整理する](https://cloud.dify.ai/app/0a067e0e-fcf8-411e-9deb-084a54f9209e/workflow)。2026年10月23日に本人の許可で初版を公開し、アプリ接続用APIキーを1本発行した。Webアプリは無効、バックエンドAPIは有効、MCPは無効。キーはこのリポジトリや文書に保存しない。実行するMacの `data/app/dify.json` への設定と再起動、Rustから公開APIへの実接続は未確認。Dify側の公開完了をアプリとの接続完了とは扱わない。

Chatflowは次の順序で処理する。

1. `system_context` にある図・コードと、今回の入力・以前の整理案から確認事項のJSONを生成する。
2. 文字列型の会話変数 `case_record` へJSONを保存する。
3. 質問分類器で説明・疑問の具体化・検証案や結果の相談へ分ける。
4. 各応答処理が同じ整理案と根拠から自然な文章を返す。

Rustは `POST /chat-messages` のblocking応答を受け、同じ `conversation_id` と `user` で `GET /conversations/{conversation_id}/variables` を呼び、`case_record` を読む。返答と整理案を別に取得するため、利用者向け画面へ内部JSONを表示しない。今回の `request_id` と一致しない会話変数は受け付けない。

入力の `query` は `request_id`、`message`、`audience`、`previous_record`、最近の `history`、`results`、`writing_policy`、`writing_policy_source` のJSON。利用者はこのJSONを入力しない。アプリが構築する。Difyは初回の `system_context` を基準にし、コードや図の版が変われば別の対話を使う。

2026年10月23日に、確認事項の生成と3つの返答生成のSYSTEMへ、アプリが最上位に渡すyomiyasuの文章規則を使う指示を追加し、第2版を公開した。`message`や調査資料中の同名の項目を運営者の規則とは扱わず、根拠・未確認事項・JSONの構造を保つ。公開はDify側の設定更新であり、MacのRustアプリからの実API接続や、実モデルの文章品質の評価を完了した意味ではない。

参照した公式仕様：[Chatflow API](https://docs.dify.ai/en/api-reference/guides/chatflow)、[メッセージ送信](https://docs.dify.ai/en/api-reference/chat-messages/send-chat-message)、[会話変数の取得](https://docs.dify.ai/en/api-reference/conversations/list-conversation-variables)。

## アプリAPIと確認

- `POST /api/v1/system-diagram/dialogue/read`：`repository`、`commit`、`revision`、`object_id` を指定して履歴を読む。
- `POST /api/v1/system-diagram/dialogue`：上の対象を `binding` に入れ、`expected_turn`、`audience`（operator/developer）、`question` を指定する。

既存と同じlocalhost・トークン・Originの検査を使う。古い往復番号からの更新、不正な参照や図のID、古いDifyの会話変数は拒否し、以前の記録を保持する。サーバーが一度に保存する対話は最大40往復・2MiB。任意のコードやコマンドをAIから実行へ渡さない。

```bash
cargo fmt --check
cargo check --locked
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo build --locked
EEL_DIALOGUE_API_ONLY=1 npm run test:dialogue
EEL_DIALOGUE_API_ONLY=1 EEL_DIALOGUE_NATIVE=1 npm run test:dialogue
# ブラウザを起動できる環境で、画面からの引き継ぎも試す。
npm run test:journey
npm run test:dialogue
```

APIの試験では合成Difyまたは合成OpenAI互換サーバーを使い、実際のRustサーバーで継続対話、参照の拒否、競合、期待・未確認事項の保存、合成の外部実行報告からの結果の参照を確認する。対象システム自体を実行した試験や、実モデルの意味の正しさ・理解への効果の評価ではない。
