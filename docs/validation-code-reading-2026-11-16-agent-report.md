# エージェントの報告：OpenELIS Global 2 のコードを読んだ結果（2026年11月16日）

[コードだけを読んだ見立てと、実行の観測の比較](validation-code-reading-2026-11-16.md)で、AIエージェントが出力した全文である。内容は直していない。

- AIが書いた見立てと懸念であり、人は全体を確かめていない。確かめた範囲は、上の比較の記録にある。
- 文中の「依頼文」は、エージェントへの指示を指す。指示では、麦漆のレシピの構成に合わせて、コンテナを5つと書いた。
- パスは、アプリ本体（`app`、`DIGI-UW/OpenELIS-Global-2` のタグ `3.3.0`）と配布物（`deploy`、`DIGI-UW/openelis-docker` のコミット `6fb4a9e9`）からの相対パスである。

---

# OpenELIS Global 2 コード読解メモ（患者の新規登録・患者の検索）

- 対象: `app`（タグ 3.3.0、bcead488）と `deploy`（6fb4a9e9）。以下のパスはこの2つのディレクトリからの相対パス。
- 方法: ファイルを読んだだけ。ビルド・起動・通信はしていない。
- 根拠の書き方: 「ファイル:行」。確信の度合いは 高・中・低。
- 区別していること:
  - **リポジトリ内で確定**: 読んだコード・設定にそう書いてある。
  - **外部の既定動作に依存**: nginx / Tomcat / Spring / PostgreSQL / Docker の既定の振る舞いを前提にした推定。その製品のソースや既定設定は対象ディレクトリにないので、確かめていない。該当箇所には「※外部既定」と付けた。

先に前提を1つ。依頼文では「5つのコンテナ」とあるが、`deploy/docker-compose.yml` が定義するサービスは8つ（certs, db, oe, **fhir**, frontend, proxy, **analyzer-bridge**, **autoheal**。deploy/docker-compose.yml:30,51,76,125,161,180,216,261）。`.env` の `COMPOSE_FILE` 行はコメントアウトされている（deploy/.env:73）ので、既定では FHIR サーバも起動する。FHIR が動いているかどうかで、操作1の保存先が変わる（A-1 の保存先、C-1 を参照）。

---

## A. 処理の見立て

### 共通: 入口からアプリまでの経路

| 項目 | 見立て | 確信 | 根拠 |
|---|---|---|---|
| ブラウザが呼ぶ API のベース | `/api/OpenELIS-Global` | 高 | app/frontend/src/config.json:4、app/frontend/src/components/utils/Utils.ts:215,474 |
| proxy の振り分け | `/api/` は `https://oe.openelis.org:8443/api/` へ、それ以外は frontend へ。パスは書き換えない | 高 | deploy/configs/nginx/nginx.conf:63-71 |
| アプリ本体での受け | Tomcat のコンテキスト `/api/OpenELIS-Global`。その中のパスが `/rest/...` | 高 | app/tomcat/oe_server.xml:145-146、app/Dockerfile:141 |
| 認証 | `/rest/**` はログイン必須（フォームログインのセッション Cookie）。POST は CSRF トークン必須 | 高 | app/src/main/java/org/openelisglobal/security/SecurityConfig.java:459-463,476、Utils.ts:480-483 |
| 画面のルート | `/PatientManagement`（検索）、`/PatientManagement/new`（新規）、`/PatientManagement/<内部ID>`（編集） | 高 | app/frontend/src/App.jsx:936-941、app/frontend/src/components/patient/PatientManagement.tsx:30-43 |

補足: oe コンテナの 8443 はホストにも直接公開されている（deploy/docker-compose.yml:86-87）ので、proxy を通らない経路もある。その場合 proxy のログには残らない。

### A-1. 操作1: 患者の新規登録

#### ブラウザが送る要求

| # | いつ | メソッド・パス（proxy から見たパス） | アプリ本体でのパス | 値が載る場所 | 確信 | 根拠 |
|---|---|---|---|---|---|---|
| 1 | National ID の欄からマウスが出たとき（値が空でなければ毎回） | `GET /api/OpenELIS-Global/rest/subjectNumberValidationProvider?fieldId=nationalID&numberType=nationalId&subjectNumber=<国民識別番号>` | `/rest/subjectNumberValidationProvider` | **URL のクエリ**（`subjectNumber=`） | 高 | app/frontend/src/components/patient/CreatePatientForm.tsx:731-745,1045-1051、app/src/main/java/org/openelisglobal/common/rest/provider/CommonValidationsRestController.java:191-201 |
| 2 | 保存 | `POST /api/OpenELIS-Global/rest/PatientManagement` | `/rest/PatientManagement` | **本文（JSON）**。`nationalId`、`lastName`、`firstName`、`gender`、`birthDateForDisplay` など。ヘッダに `X-CSRF-Token` | 高 | CreatePatientForm.tsx:763-773、Utils.ts:464-486、app/src/main/java/org/openelisglobal/patient/controller/rest/PatientManagementRestController.java:64-68 |
| 3 | 保存成功後 | 画面が `/PatientManagement/<内部ID>` に移り、`GET .../rest/patient-details?patientID=<内部ID>` と `GET .../rest/patient-photos/<内部ID>/false` | 同左 | クエリ・パスに載るのは内部の連番IDだけ | 高 | CreatePatientForm.tsx:801-807、app/frontend/src/components/patient/usePatientDetails.ts:67-78 |

- 年齢は送られない。画面で「今日 − 年齢」を日付にして `birthDateForDisplay` に入れ、`years`/`months`/`days` は送信前に消している（CreatePatientForm.tsx:163-176,334-356,766-769）。確信: 高。
- 1 の要求は「保存しなかった場合」でも飛ぶ。キーボードだけで操作すると飛ばない（`onMouseOut` だけに付いている。CreatePatientForm.tsx:1045）。確信: 高。

#### 保存先

スキーマは `clinlims`（app/src/main/resources/hibernate/hibernate.cfg.xml:12）。同じトランザクションで書く（app/src/main/java/org/openelisglobal/patient/service/PatientServiceImpl.java:627-667）。

| 入力 | 表.列 | 加工 | 確信 | 根拠 |
|---|---|---|---|---|
| 国民識別番号 | `clinlims.patient.national_id`（varchar(255)） | **そのまま（平文）**。サーバ側の検査は正規表現 `(?i)^[-a-z0-9/]*$` と必須チェックだけ。暗号化・ハッシュ化のコードは経路上に見当たらない | 高 | app/src/main/resources/hibernate/hbm/Patient.hbm.xml:6-7,50-52、app/db/dbInit/OpenELIS-Global.sql:3758-3779、app/src/main/java/org/openelisglobal/patient/action/bean/PatientManagementInfo.java:81-85、app/src/main/java/org/openelisglobal/common/validator/ValidationHelper.java:16、app/src/main/java/org/openelisglobal/patient/util/PatientUtil.java:180-181 |
| 姓・名 | `clinlims.person.last_name` / `first_name` | そのまま | 高 | app/src/main/resources/hibernate/hbm/Person.hbm.xml:7,20,23、PatientServiceImpl.java:647-648 |
| 性別 | `clinlims.patient.gender`（1文字、M/F） | そのまま | 高 | Patient.hbm.xml:28-30、ValidationHelper.java:18 |
| 年齢 | `clinlims.patient.entered_birth_date`（文字列）と `birth_date`（timestamp） | **年齢そのものは保存されない**。画面が逆算した日付が入る | 高 | Patient.hbm.xml:32-34,74-77、app/src/main/java/org/openelisglobal/patient/valueholder/Patient.java:261-266 |

ほかに書き込まれる表:

| 表 | 内容 | 確信 | 根拠 |
|---|---|---|---|
| `clinlims.patient_identity` | 種別 GUID の行（サーバが作る UUID）。国民識別番号はここには**入らない**（`persistIdentityTypes` の一覧に国民識別番号の種別がない）。画面で他の欄を入れればその種別の行も増える | 高 | PatientServiceImpl.java:654-658,709-727,842-851 |
| `clinlims.history`（監査） | person・patient・patient_identity の INSERT ごとに1行。誰が（`sys_user_id`）、いつ、どの表のどの行か。**値は入らない**（INSERT のときは `changes` が空） | 高 | PatientServiceImpl.java:198-201、app/src/main/java/org/openelisglobal/common/service/AuditableBaseObjectServiceImpl.java:33-39、app/src/main/java/org/openelisglobal/audittrail/daoimpl/AuditTrailServiceImpl.java:63-66,99-122、OpenELIS-Global.sql:1568-1576,15029-15030 |
| `person_address` | 入力する5項目だけなら、空の値は挿入しない分岐になっている | 高 | PatientServiceImpl.java:787-793,803-807 |
| `patient_contact` と `person`（2行目） | 画面は中身が空の連絡先（`patientContact.person`）を常に送る。サーバが保存を飛ばすのは `person` が null のときだけなので、**空の person 1行と patient_contact 1行が入る**と推定。どちらも監査の対象なので `history` にも行が増える | 中（JSON の変換結果までは追っていない。C-8） | PatientServiceImpl.java:674-688、app/frontend/src/components/formModel/innitialValues/CreatePatientFormValues.js:31-38、app/src/main/java/org/openelisglobal/patient/util/PatientUtil.java:195 |
| Lucene の索引（oe コンテナの `/var/lib/lucene_index`） | `Patient.nationalId` は索引対象の注釈が付いているが、自動索引は無効。保存時には書かれない。`GET /rest/reindex` を呼んだときだけ作られる | 高（無効化の設定）／中（実行時に本当に書かれないか） | app/src/main/java/org/openelisglobal/patient/valueholder/Patient.java:62-63、app/src/main/resources/persistence/persistence.xml:256-279 |
| **FHIR ストア（別コンテナ `fhir.openelis.org`）** | 保存の直後に、非同期で FHIR の Patient を作って送る。国民識別番号は identifier に入る。FHIR サーバは同じ PostgreSQL の同じ DB・スキーマに接続する設定なので、FHIR 側の表にも国民識別番号が入るはず。**FHIR 側の表名・列名は対象ディレクトリにコードがなく、分からない** | 中（送ること自体は 高。FHIR が動いているかは実行時しだい） | PatientManagementRestController.java:106-107、app/src/main/java/org/openelisglobal/dataexchange/fhir/service/FhirTransformServiceImpl.java:369-403、app/src/main/java/org/openelisglobal/fhir/service/PatientTransformServiceImpl.java:142,151、deploy/configs/properties/common.properties:15、deploy/docker-compose.yml:152、app/fhir/hapi_application.yaml:17 |

#### ログへの出力

「出る」と書いたものは、入力した値（国民識別番号・氏名）が文字として現れる、という意味。

| コンテナ | 標準出力・標準エラー | コンテナ内のファイル | 確信 | 根拠 |
|---|---|---|---|---|
| **proxy** | 要求 1 の GET の要求行（クエリ込み＝国民識別番号）がアクセスログとして標準出力に出る、と推定。POST の本文は出ない | なし（と推定） | 中 ※外部既定 | deploy/configs/nginx/nginx.conf:1-73 に `access_log` の指定が**ない**（＝nginx の既定が効く）。イメージは `nginx:1.15-alpine` に healthcheck を足しただけ（app/nginx-proxy/Dockerfile:1-11）。「既定の形式に要求行が入る」「公式イメージはアクセスログを標準出力に流す」は外部の知識 |
| **oe** | 通常の成功時は出ない。INFO で出るのは監査の「Created INSERT history record for table: …, referenceId: <内部ID>」など、内部IDだけ。**検証エラーのとき**は ERROR に入力値が出うる（B-4） | (a) **Tomcat アクセスログ** `/usr/local/tomcat/logs/localhost_access_log.*.txt` に要求 1 の要求行（国民識別番号）が出る。ホストの `deploy/configs/logs/tomcatLogs/` に見える。(b) `/var/lib/openelis-global/logs/openELIS.log` は標準出力と同じ内容（ホストの `deploy/configs/logs/oeLogs/`） | (a) 高、(b) 高（設定）、ERROR 時の中身は 中 | app/tomcat/oe_server.xml:164-166（`pattern="%h %l %u %t "%r" %s %b"`）、deploy/docker-compose.yml:112-113、app/src/main/resources/log4j2.properties:4-21,29-39、AuditTrailServiceImpl.java:120-121 |
| **db** | 出ない、と推定。PostgreSQL の設定を変えるファイルが db イメージにない（＝文の記録は既定で無効）。アプリはバインド変数で値を渡す | データファイルと WAL には当然入る（ホストの `deploy/configs/database/data/`） | 中 ※外部既定 | app/db/Dockerfile:2-16（postgresql.conf を触っていない）、deploy/docker-compose.yml:63-65。実際の `postgresql.conf` はデータ用ボリュームの中にあり、読めていない |
| **frontend** | 出ない。frontend に届くのは `/api/` 以外（画面の HTML・JS、`/PatientManagement/new`、`/PatientManagement/<内部ID>` の再読み込みなど）だけ | なし（と推定） | 高（値が届かないこと）／中（ログの出先 ※外部既定） | deploy/configs/nginx/nginx.conf:63-66、app/frontend/nginx/nginx.conf:1-36、app/frontend/Dockerfile:44-47 |
| （ブラウザ） | GET が失敗したときだけ、コンソールに `GET <クエリ込みのパス> failed` が出る | — | 高 | Utils.ts:236 |

- oe の SQL ログ: `application.properties` に `spring.jpa.show-sql=true` などがある（app/src/main/resources/application.properties:45-48）が、pom に spring-boot がなく（app/pom.xml を grep）、Hibernate は `persistence.xml` と `hibernate.cfg.xml`（`show_sql=false`。hibernate.cfg.xml:16）で組み立てている（app/src/main/java/org/openelisglobal/config/HibernateConfig.java:27-36）。この設定は効いておらず、SQL と値はログに出ないと推定。確信: 中。
- FHIR へ送る内容の全文を出すログ行があるが TRACE で、既定の INFO では出ない（app/src/main/java/org/openelisglobal/dataexchange/fhir/service/FhirPersistanceServiceImpl.java:153-157、log4j2.properties:30）。確信: 高。
- Docker 側: db・oe・frontend・proxy とも `local` ドライバで 20MB×50 ファイルまで保持（deploy/docker-compose.yml:3-8,67,92,177,205）。標準出力に出たものは、ホストの Docker のログ領域にこの量まで残る。確信: 高。

#### 画面への表示

- 成功: 「保存しました」の通知のあと、同じ患者の編集画面（`/PatientManagement/<内部ID>`）に移る。確信: 高（CreatePatientForm.tsx:779-807）。
- 国民識別番号が既存と一致: 欄からマウスが出た時点で「nationalId:<入力値> Already in use」の通知。保存は止めない（B-6）。確信: 高（CreatePatientForm.tsx:707-729）。

### A-2. 操作2: 患者の検索

#### ブラウザが送る要求

| # | いつ | メソッド・パス | 値が載る場所 | 確信 | 根拠 |
|---|---|---|---|---|---|
| 1 | 検索ボタン | `GET /api/OpenELIS-Global/rest/patient-search-results?lastName=&firstName=&STNumber=<ID>&subjectNumber=<ID>&nationalID=<ID>&labNumber=&guid=&dateOfBirth=&gender=&suppressExternalSearch=true`（アプリ本体では `/rest/patient-search-results`） | **URL のクエリ。Patient ID の欄の値が3か所**（`STNumber`・`subjectNumber`・`nationalID`）に同じ値で載る。`encodeURIComponent` は使っておらず、文字列の連結 | 高 | app/frontend/src/components/patient/SearchPatientForm.tsx:186-222、Utils.ts:209-226、app/src/main/java/org/openelisglobal/common/rest/provider/PatientSearchRestController.java:72-82 |
| 2 | ページ送り | 1 と同じ URL に `&page=<n>` | 同上（ID がもう一度 URL に載る） | 高 | SearchPatientForm.tsx:225-228、PatientSearchRestController.java:86,125-127 |
| 3 | 結果の各行 | `GET .../rest/patient-photos/<内部ID>/true`（写真のサムネイル） | パスに内部IDだけ | 高 | SearchPatientForm.tsx:668-672、app/frontend/src/components/patient/photoManagement/photoAvatar/AyncAvatar.tsx:45-46 |
| 4 | 行のラジオボタンを選ぶ | `GET .../rest/patient-details?patientID=<内部ID>` | クエリに内部IDだけ | 高 | SearchPatientForm.tsx:336-339 |

#### 保存先（読み出しと、書き込みの有無）

- 読む表: `clinlims.patient`、`person`、`patient_identity`（ST・SUBJECT・GUID の3回結合）、加えて `observation_history`（紹介元の患者ID での一致）と、統合済み患者の確認。確信: 高（app/src/main/java/org/openelisglobal/sample/daoimpl/DBSearchResultsDAOImpl.java:363-377、app/src/main/java/org/openelisglobal/common/provider/query/workerObjects/PatientSearchLocalWorker.java:94-99、app/src/main/java/org/openelisglobal/search/service/DBSearchResultsServiceImpl.java:34-47）。
- 使われる実装は DB 版（`@Primary`）。Lucene 版は使われない。確信: 高（DBSearchResultsDAOImpl.java:37-39、DBSearchResultsServiceImpl.java:19-21）。
- 一致のしかた: 入力値の前後に `%` を付け、`ilike` で **部分一致・大文字小文字無視**。対象は「subject number または national_id または external_id または ST number」。確信: 高（DBSearchResultsDAOImpl.java:135-138,379-402）。
- **DB への書き込みはない**。検索したこと自体の記録（誰が何で検索したか）を書くコードは経路上に見当たらない。確信: 中（「ない」ことの確認なので。読んだ範囲は D を参照）。
- 結果の全件（全ページ分）を HTTP セッションに入れている（oe の JVM のメモリ）。確信: 高（app/src/main/java/org/openelisglobal/common/rest/util/PatientSearchResultsPaging.java:20-30、app/src/main/java/org/openelisglobal/common/paging/PagingUtility.java:40-46）。

#### ログへの出力

| コンテナ | 標準出力・標準エラー | コンテナ内のファイル | 確信 | 根拠 |
|---|---|---|---|---|
| **proxy** | GET の要求行（Patient ID が3回入ったクエリ）がアクセスログとして標準出力に出る、と推定 | なし（と推定） | 中 ※外部既定 | A-1 と同じ |
| **oe** | アプリのログには出ない。INFO で出るのは「Requesting page 1, totalPages=…」だけ。SQL 文を出す行は TRACE（値は含まない） | **Tomcat アクセスログに要求行（Patient ID 入り）が出る**。ホストの `deploy/configs/logs/tomcatLogs/` | 高 | app/tomcat/oe_server.xml:164-166、PagingUtility.java:65-67、DBSearchResultsDAOImpl.java:176-179、log4j2.properties:30 |
| **db** | 出ないと推定（A-1 と同じ理由） | — | 中 ※外部既定 | A-1 と同じ |
| **frontend** | 出ない。検索の要求は `/api/` なので frontend に届かない | — | 高 | deploy/configs/nginx/nginx.conf:63-71 |

アクセスログ1行の形（oe）: `<接続元IP> - <ユーザー> [時刻] "GET /api/OpenELIS-Global/rest/patient-search-results?...&nationalID=<ID>... HTTP/1.1" 200 <バイト数>`。接続元IPは proxy コンテナのアドレスになるはず（`server.xml` に転送ヘッダを読む Valve がない）。`<ユーザー>` が実際のログイン名になるか `-` になるかは分からない（C-3）。

#### 画面への表示

結果の表の列は次の7つ（app/frontend/src/components/data/PatientResultsTableHeaders.tsx:4-12、SearchPatientForm.tsx:609-755）。確信: 高。

| 列 | 中身 | 出どころ |
|---|---|---|
| （先頭） | ラジオボタン、写真の丸アイコン、統合済みなら「Merged」タグ | SearchPatientForm.tsx:656-700 |
| Last Name | `person.last_name` | DBSearchResultsDAOImpl.java:363-365 |
| First Name | `person.first_name` | 同上 |
| Gender | `patient.gender`（M/F） | 同上 |
| Date of Birth | `patient.entered_birth_date`（年齢で登録した患者は逆算した日付） | 同上、app/src/main/java/org/openelisglobal/common/provider/query/PatientSearchResults.java:84-86 |
| Unique Health ID number | `patient_identity` の SUBJECT | 同上 |
| **National ID** | `patient.national_id`（**全桁そのまま**。伏せ字にする処理はない） | 同上、SearchPatientForm.tsx:743-745 |
| Data Source Name | 「OpenElis」のタグ | PatientSearchLocalWorker.java:102-106、app/src/main/resources/languages/message_en.properties:4307 |

- 部分一致なので、入力した番号を**含む**患者がすべて並ぶ。1ページ最大99件前後（deploy/configs/properties/common.properties:64）。確信: 高。
- 応答の JSON には、表に出していない項目（内部ID、GUID、ST 番号、external ID、紹介元の患者ID など）も入っている。確信: 高（PatientSearchResults.java:25-44）。

---

## B. 懸念

### B-1. 国民識別番号・Patient ID が URL のクエリに載り、平文でアクセスログに残る

- **何がマズいか**: 検索のたびに、また新規登録の重複チェックのたびに、識別番号が GET のクエリに載る。oe の Tomcat アクセスログ（ホストのディレクトリに出るファイル）に必ず残り、proxy の標準出力（Docker のログ）にも残ると推定される。DB の外に識別番号の写しが増え、消す仕組みは設定に見当たらない。
- **根拠**:
  - 検索: `"&nationalID=" + values.patientId`（SearchPatientForm.tsx:190-211）
  - 登録: `` `/rest/subjectNumberValidationProvider?fieldId=${fieldId}&numberType=${numberType}&subjectNumber=${numberValue}` ``（CreatePatientForm.tsx:738-739）。同じ形で電話番号も `PhoneNumberValidationProvider?...&value=` に載る（CreatePatientForm.tsx:542-543）
  - oe: `<Valve className="org.apache.catalina.valves.AccessLogValve" directory="logs" ... pattern="%h %l %u %t "%r" %s %b" />`（app/tomcat/oe_server.xml:164-166）、`- ./configs/logs/tomcatLogs/:/usr/local/tomcat/logs`（deploy/docker-compose.yml:113）
  - proxy: `nginx.conf` に `access_log` の行がない（deploy/configs/nginx/nginx.conf:1-73）。Docker のログは `max-size: "20m"`, `max-file: "50"`（deploy/docker-compose.yml:3-8）
- **確信**: oe のファイルは 高（Valve の設定とマウントが書いてある。`%r` が要求行であることだけ Tomcat の仕様に依存）。proxy の標準出力は 中（nginx の既定と公式イメージの作りに依存）。
- **種類**: oe は「コードから確定できる」。proxy は「コードからは推定。設定や実行時の状態で変わる」。
- **確かめ方**: 目印になる番号で検索と登録（National ID 欄からマウスを外す）を行い、(1) ホストの `deploy/configs/logs/tomcatLogs/localhost_access_log.*.txt`、(2) `docker logs openelisglobal-proxy`、(3) `docker logs openelisglobal-webapp` をその番号で grep する。あわせて、アクセスログのファイルが日がたっても消えないかを見る（保持日数の指定が `server.xml` にない）。

### B-2. 患者検索の API は、役割に関係なくログインしていれば誰でも呼べる

- **何がマズいか**: 画面では検索ページを受付の役割に限っているが、サーバ側は `/rest/patient-search-results` を「ログイン済みなら許可」にしている。受付の権限がないアカウントでも、API を直接呼べば氏名・生年月日・国民識別番号を引ける。
- **根拠**:
  - サーバ: `// SECURITY NOTE: REST endpoints without SystemModuleUrl DB entries are auto-allowed for any authenticated user.` に続いて `if (isRestFullPath(path)) { return true; }`（app/src/main/java/org/openelisglobal/interceptor/ModuleAuthenticationInterceptor.java:101-108）
  - 照合は `/rest` を外したパスと `system_module_url.url_path` の完全一致（app/src/main/java/org/openelisglobal/common/util/URLUtil.java:25-27、app/src/main/java/org/openelisglobal/systemmodule/daoimpl/SystemModuleUrlDAOImpl.java:27-43）
  - 初期データと全マイグレーションを `patient-search-results` で検索して、該当する行が1つもない（app/db/dbInit/、app/src/main/resources/liquibase/）。コントローラに `@PreAuthorize` もない（PatientSearchRestController.java:49-51,72-74）
  - 対照: 保存の `/rest/PatientManagement` は `/PatientManagement` の行（モジュール46）があるので役割で絞られる（OpenELIS-Global.sql:16585）
  - 画面: `<SecureRoute path="/PatientManagement/:patientId?" ... role={Roles.RECEPTION} />`（App.jsx:936-941）
- **確信**: 高（コードと初期データ）。運用中の DB に管理者が行を足していれば変わるので、実機については 中。`/rest/patient-details`、`/rest/subjectNumberValidationProvider` も同じ理由で同じ状態と推定（確信: 中。行がないことだけ確認）。
- **種類**: コードからは推定。設定や実行時の状態で変わる（`system_module_url` の中身しだい）。
- **確かめ方**: 受付の役割を持たないユーザー（たとえば結果入力だけの役割）でログインし、同じブラウザで `/api/OpenELIS-Global/rest/patient-search-results?nationalID=1&suppressExternalSearch=true` を開く。200 と患者の一覧が返れば該当。開発者には「コメントにある per-role module mappings は患者検索について予定があるか」を聞く。

### B-3. 検索が部分一致・最小文字数なしで、1文字でも大量の患者が並ぶ。閲覧の記録もない

- **何がマズいか**: Patient ID 欄は「含む」検索で、文字数の下限がない。`1` と入れれば、国民識別番号などに 1 を含む患者が氏名・生年月日・国民識別番号つきで全員返る（全件をセッションに持ち、ページ送りで順に取れる）。`_`（任意の1文字）もエスケープされないので、番号が入っている患者をほぼ全員引ける。そのうえ、誰が何を検索して誰を見たかは、B-1 のアクセスログ以外に残らない。
- **根拠**:
  - `nationalID = '%' + nationalID + '%';`（DBSearchResultsDAOImpl.java:135-138）、`queryBuilder.append(" p.national_id ilike :");`（同 386-390）
  - 空でないことだけを見ている（PatientSearchLocalWorker.java:83-90）。画面側も検証なし（`// validationSchema={}`。SearchPatientForm.tsx:377）
  - 全件をセッションへ: `paging.setDatabaseResults(request.getSession(), results, pagingHelper);`（PatientSearchResultsPaging.java:24）
  - 監査は INSERT/UPDATE/DELETE のときだけ（AuditableBaseObjectServiceImpl.java:33-39,106-119,144-145）
- **確信**: 部分一致と下限なしは 高。`_` が通ることは 高（バインド変数にそのまま入る。途中で落とす処理は見当たらない）。「閲覧の記録がない」は 中（ないことの確認）。
- **種類**: コードから確定できる（閲覧記録がない点だけ、読んでいない範囲に別の仕組みがある可能性を残す）。
- **確かめ方**: 患者を数人登録して Patient ID に `1`、次に `_` を入れて検索し、返る件数を見る。検索の前後で `clinlims.history` の行数が変わらないことを見る。開発者には「患者の閲覧・検索の監査は要件にあるか」を聞く。

### B-4. 保存が検証エラーになると、入力値や既存患者の氏名・生年月日が ERROR ログに出うる

- **何がマズいか**: 保存時にサーバ側の検証で落ちると、検証結果をそのまま例外のメッセージにしてログへ出している。落ちた欄の入力値（国民識別番号や氏名）が入ると推定される。重複を禁止する設定のときは、同じ番号を持つ**既存患者の氏名と生年月日**を入れたメッセージを作っており、これもログに入る。出先は oe の標準出力と `openELIS.log`（ホストのディレクトリ）。
- **根拠**:
  - `LogEvent.logError(new BindException(bindingResult));`（PatientManagementRestController.java:86-90）
  - `.append(", Message: ").append(sanitizeLogMessage(throwable.getMessage()))`（app/src/main/java/org/openelisglobal/common/log/LogEvent.java:70-79）。`sanitizeLogMessage` は改行の置換と HTML エスケープだけで、値は伏せない（同 221-227）
  - `errors.reject("error.duplicate.nationalId", null, duplicateNationalIdMessage(newNationalId, result));`（app/src/main/java/org/openelisglobal/patient/validator/ValidatePatientInfo.java:63-70）、文面は `National ID {0} already belongs to patient {1}, born {2}`（message_en.properties:2377-2381）
  - 出先: console と `${basePath}/openELIS.log`（log4j2.properties:4-21,29-34）、`./configs/logs/oeLogs:/var/lib/openelis-global/logs`（deploy/docker-compose.yml:112）
- **確信**: 中。「検証結果の文字列に、落ちた欄の値と既定メッセージが入る」は Spring の `BindException`／`FieldError` の作りに依存していて、リポジトリ内では確かめられない。ログに出す行があること自体は 高。
- **種類**: コードからは推定。設定や実行時の状態で変わる（重複の件は `ALLOW_DUPLICATE_NATIONAL_IDS=false` のときだけ）。
- **確かめ方**: (1) API に直接、`nationalId` に `A B`（空白入り。正規表現に合わない）を入れて POST し、`docker logs openelisglobal-webapp` と `deploy/configs/logs/oeLogs/openELIS.log` に `rejected value [A B]` が出るかを見る。(2) 管理画面で国民識別番号の重複を禁止にし、既存と同じ番号で保存して、ログに既存患者の氏名・生年月日が出るかを見る。同じ文面は 400 の応答にも入るので、画面の通知にも出るはず（PatientManagementRestController.java:91-98）。

### B-5. 年齢だけを入れると、逆算した日付が「生年月日」として保存され、推定である印が残らない

- **何がマズいか**: 年齢 40 を入れると「今日のちょうど40年前の日付」が生年月日として保存される。年齢そのものも、推定だという印も送られない。検索結果の Date of Birth 列にも、FHIR に送る患者にも、本物の生年月日と区別のつかない形で出る。生年月日は患者の同定や年齢別の基準値に使われるので、誤った一致・不一致のもとになる。
- **根拠**:
  - `pastDate.setFullYear(currentDate.getFullYear() - (Number(years) || 0));` … `return format(new Date(pastDate), dateLocale === "fr-FR" ? "dd/MM/yyyy" : "MM/dd/yyyy");`（CreatePatientForm.tsx:163-176）
  - `delete payload.years; delete payload.months; delete payload.days;`（同 766-769）
  - サーバはその文字列を `entered_birth_date` に入れ、`birth_date` に変換する（Patient.java:261-266、Patient.hbm.xml:32-34,74-77）
  - サーバ側には「日や月が不明」を表す書き方（`xx/xx/yyyy`）を受ける検証があるが、画面は使っていない（ValidatePatientInfo.java:95-106）
- **確信**: 高（画面のコードと保存の経路がそう書いてある）。「不明の日を表す書き方を画面のどこかで作っているか」は CreatePatientForm.tsx 全体を読み切っていないので 中。
- **種類**: コードから確定できる。
- **確かめ方**: 年齢だけ（たとえば 40）で登録し、`select entered_birth_date, birth_date from clinlims.patient` と、検索結果の Date of Birth 列を見る。翌日に同じ年齢で別の患者を登録すると、生年月日が1日ずれることも確認できる。開発者には「年齢入力の患者を推定生年として区別する設計はあるか」を聞く。

### B-6. 同じ国民識別番号の患者が複数できる。止めるのは、マウスを外したときの通知だけ

- **何がマズいか**: 新規インストールの既定では重複が許可になっていると推定され、その場合サーバは同じ番号を拒まない。画面の重複チェックは欄からマウスが出たときだけ走り、通知を出すだけで保存を止めない。Tab キーで移動すればチェック自体が走らない。DB にも一意制約がない。患者の取り違えにつながる。
- **根拠**:
  - 既定値: マイグレーションが `Allow duplicate national ids` を `true` で入れる（app/src/main/resources/liquibase/2.3.x.x/national_id_config.xml:27-38）。DB の値はコードの既定（`false`。app/src/main/java/org/openelisglobal/common/util/DefaultConfigurationProperties.java:311）より優先される（同 97-108）
  - サーバの拒否は禁止設定のときだけ（ValidatePatientInfo.java:22-26,63-70）
  - 画面: `onMouseOut={() => { handleSubjectNoValidation("nationalId", "nationalID", values.nationalId); }}`（CreatePatientForm.tsx:1045-1051）。コールバックの中で `error = "duplicate"` を返しているが、呼び出し側はその値を受け取れない（非同期のコールバックの戻り値。同 707-745）
  - DB: `national_id` には検索用の索引だけ（app/src/main/resources/liquibase/3.5.x.x/090-patient-search-trigram-indexes.xml の `idx_patient_national_id_trgm`）。一意制約は初期スキーマにない（OpenELIS-Global.sql:3758-3779）
  - 重複チェックのコントローラは、応答オブジェクトを1つだけ持って全要求で使い回している（`private ResponseObject responseObject;`。CommonValidationsRestController.java:35,42-44,203）。同時に来た別の要求の結果と混ざる余地がある
- **確信**: 画面が保存を止めないこと・一意制約がないことは 高。既定が「重複許可」であることは 中（設定の読み込み順を読んだ結果。ホストの `TotalSystemConfiguration.properties` があればそちらが勝つ）。応答の混線は 低（競合が実際に起きるかは動かさないと分からない）。
- **種類**: コードからは推定。設定や実行時の状態で変わる。
- **確かめ方**: 管理画面で「Allow duplicate national ids」の現在値を見る。同じ番号で2人、キーボードだけで入力して保存し、2人とも保存されるかを見る。禁止設定にして同じことをする。開発者には「重複許可を既定にしている理由」を聞く。

### B-7. FHIR への送信は非同期で、失敗しても画面は「保存しました」になる

- **何がマズいか**: 患者を DB に保存したあとの FHIR ストアへの送信は別スレッドで走る。失敗はログに出るだけで、利用者には成功と表示される。コントローラには FHIR の例外を受けて 500 を返す分岐があるが、非同期なのでそこには届かない。OpenELIS の DB と FHIR ストアで患者がずれたままになりうる。
- **根拠**:
  - `@Override @Async @Transactional(readOnly = true) public void transformPersistPatient(...)`（FhirTransformServiceImpl.java:369-373）、`@EnableAsync`（app/src/main/java/org/openelisglobal/config/AsyncConfig.java:11-25）
  - 失敗時はログだけ（app/src/main/java/org/openelisglobal/config/AsyncExceptionHandler.java:9-13）
  - 届かない分岐: `} catch (FhirTransformationException | FhirPersistanceException e) {`（PatientManagementRestController.java:119-122）
  - FHIR を止めた構成（light）では `localFhirClient` が作られず（FhirPersistanceServiceImpl.java:69-74）、保存のたびに例外がログに出ると推定（同 152-161）
- **確信**: 非同期で例外が呼び出し元に戻らないことは 中〜高（Spring の `@Async` の作りに依存）。あとから再送する仕組みがあるかどうかは読んでいない（C-5）。
- **種類**: コードからは推定。設定や実行時の状態で変わる。
- **確かめ方**: FHIR コンテナを止めて患者を登録し、画面が成功になるか、oe のログに何が出るかを見る。FHIR を戻したあと、その患者が FHIR ストアに現れるかを見る。開発者には再送の仕組みの有無を聞く。

### 補足（2つの操作そのものではなく、保存先の守りの話）

懸念の数には入れないが、保存先を調べる途中で目に入ったので書いておく。国民識別番号は平文で DB に入る（A-1）。その DB はホストの 15432 番に公開されていて（deploy/docker-compose.yml:55-56）、配布物の既定のパスワードは `clinlims`／`superuser`／`admin`（deploy/.env:4,6、deploy/configs/database/database.env:2、app/db/dbInit/1-pgsqlPermissions.sql.template:1-2）、README は変更を「Optional」としている（deploy/README.md:33-40）。確信: 高（設定の記載）。実際に外から届くかは、ホストのファイアウォールしだい。

---

## C. コードだけでは分からなかったこと

1. **FHIR サーバが動いている構成かどうか**。依頼文の「5つのコンテナ」と compose の定義（8サービス）が合わない。light 構成（deploy/docker-compose.light.yml）なら FHIR はなく、国民識別番号の写しは FHIR 側にできない。決めるのに必要: 実行時の `docker compose ps`、`.env` の `COMPOSE_FILE`。FHIR ありの場合、FHIR 側のどの表に入るかは FHIR サーバ（別イメージ）の中身が必要。
2. **proxy と frontend のアクセスログの出先と形式**。`nginx.conf` に指定がないことまでしか言えない。必要: 配布イメージ `itechuw/openelis-global-2-proxy:3.3.0` の中の `/var/log/nginx` の実体、または `docker logs`。配布イメージが `app/nginx-proxy/Dockerfile` から作られたものと同じかも未確認（イメージのタグとソースのタグが同じ 3.3.0 というだけ）。同じことは oe・db・frontend のイメージにも言える。
3. **Tomcat アクセスログの `%u`（ユーザー）欄と保持期間**。Spring Security のログイン名がここに出るのか `-` になるのか、ファイルが何日残るのかは Tomcat の実装・既定しだい。必要: 実際のログ1行と、数日後のファイル一覧。
4. **PostgreSQL のログ設定**。実際の `postgresql.conf` は `deploy/configs/database/data/` に初回起動時に作られ、リポジトリにない。エラーになった文や遅い文が記録される設定になっていれば、検索語がログに出る可能性がある。必要: `show log_statement; show log_min_duration_statement; show log_parameter_max_length_on_error;` の結果。
5. **FHIR 送信の再送・整合の仕組み**（B-7）。`common.properties` に「失敗した FHIR エクスポートの再試行」の間隔の設定がある（deploy/configs/properties/common.properties:20-24）が、患者の保存がその対象かは追っていない。
6. **実運用 DB の `system_module_url` と役割の割り当て**（B-2）。初期データとマイグレーションしか読めない。必要: `select url_path, system_module_id from clinlims.system_module_url` と、各役割に付いたモジュール。
7. **サイト設定の実際の値**（`Allow duplicate national ids`、`National ID required`、`UseExternalPatientInfo`、`ENABLE_CLIENT_REGISTRY`、日付の形式）。初期値は読めるが、管理画面や `TotalSystemConfiguration.properties`（oe が起動時に書き出す。deploy/.gitignore に記載）で変わる。外部検索やクライアントレジストリが有効だと、検索語が外部のサーバにも送られる（PatientSearchRestController.java:110-120,155-163）が、既定では無効と読んだので見立てから外した。
8. **`patient_contact` に空の行が入るか**（A-1）。画面が空の連絡先を送り、サーバは `getPerson() == null` のときだけ保存を飛ばす（PatientServiceImpl.java:674-688）。コード上は入ると読めるが、JSON の変換結果しだいなので、登録の前後で `select count(*) from clinlims.patient_contact` と `clinlims.person` を比べるのが早い。
9. **Patient ID に `+`・`&`・`#`・`%` を入れたときの挙動**。画面が URL エンコードをしていないので、値が変わったり欠けたりするはずだが、ブラウザと Tomcat の扱いしだい。登録できる国民識別番号は英数字と `-` `/` だけなので、通常の番号では問題にならないと見ている。
10. **応答のキャッシュ制御ヘッダ**。検索結果の JSON がブラウザや途中の機器にキャッシュされないかは、Spring Security の既定ヘッダに依存する。実際の応答ヘッダを見る必要がある。

---

## D. 読んだ範囲

### 読んだ主なファイル

配布物（deploy）:
- `docker-compose.yml`、`docker-compose.light.yml`、`.env`、`.gitignore`
- `configs/nginx/nginx.conf`、`configs/database/database.env`、`configs/properties/common.properties`、`configs/properties/SystemConfiguration.properties`、`configs/certs/ensure-stores.sh`
- `README.md`（パスワード・ポート・light 構成の箇所を grep で確認しただけ）

アプリ本体（app）— コンテナの作り:
- `Dockerfile`、`tomcat/oe_server.xml`、`install/docker-entrypoint.sh`、`install/tomcat-resources/logging.properties`
- `nginx-proxy/Dockerfile`、`frontend/Dockerfile`、`frontend/nginx/nginx.conf`
- `db/Dockerfile`、`db/docker-entrypoint-collation.sh`、`db/dbInit/init.sh`、`db/dbInit/1-pgsqlPermissions.sql.template`、`db/dbInit/OpenELIS-Global.sql`（`patient`・`history` の定義、`reference_tables`・`system_module_url` の初期データを grep）
- `fhir/Dockerfile`、`fhir/hapi_application.yaml`（先頭）、`tomcat/hapi_server.xml`（コネクタの行だけ）

アプリ本体 — 画面（`frontend/src`）:
- `components/patient/PatientManagement.tsx`、`SearchPatientForm.tsx`（全体）、`CreatePatientForm.tsx`（送信・重複チェック・年齢の計算・国民識別番号の欄。全 2127 行のうち約 350 行）、`usePatientDetails.ts`、`types.ts`
- `components/utils/Utils.ts`（GET/POST の関数）、`components/data/PatientResultsTableHeaders.tsx`、`components/formModel/innitialValues/*PatientFormValues`、`config.json`、`App.jsx`（ルートの箇所）、`public/service-worker.js`

アプリ本体 — サーバ（`src/main`）:
- 登録: `patient/controller/rest/PatientManagementRestController.java`、`patient/util/PatientUtil.java`、`patient/validator/ValidatePatientInfo.java`、`patient/service/PatientServiceImpl.java`（保存の部分）、`patient/action/bean/PatientManagementInfo.java`（先頭140行）、`patient/valueholder/Patient.java`（一部）、`common/rest/provider/CommonValidationsRestController.java`
- 検索: `common/rest/provider/PatientSearchRestController.java`、`common/provider/query/workerObjects/PatientSearchLocalWorker.java`、`search/service/DBSearchResultsServiceImpl.java`、`sample/daoimpl/DBSearchResultsDAOImpl.java`、`sample/daoimpl/LuceneSearchResultsDAOImpl.java`、`common/rest/util/PatientSearchResultsPaging.java`、`common/provider/query/PatientSearchResults.java`、`common/rest/provider/PatientSearchPopulateRestController.java`（一部）
- 監査・ログ: `audittrail/daoimpl/AuditTrailServiceImpl.java`（前半）、`common/service/AuditableBaseObjectServiceImpl.java`、`common/log/LogEvent.java`、`resources/log4j2.properties`、`resources/application.properties`
- 権限: `security/SecurityConfig.java`（フィルタチェーンの部分）、`interceptor/ModuleAuthenticationInterceptor.java`、`common/util/URLUtil.java`、`systemmodule/daoimpl/SystemModuleUrlDAOImpl.java`、`config/AppConfig.java`（インターセプタの登録）、`security/CORSFilter.java`（どこにも登録されていないことを確認。懸念から外した）
- 永続化の設定: `resources/hibernate/hibernate.cfg.xml`、`hbm/Patient.hbm.xml`、`Person.hbm.xml`、`PatientIdentity.hbm.xml`、`History.hbm.xml`、`resources/persistence/persistence.xml`（末尾）、`config/HibernateConfig.java`、`webapp/META-INF/context.xml`、`webapp/WEB-INF/web.xml`（セッションとフィルタの箇所）
- FHIR: `dataexchange/fhir/service/FhirTransformServiceImpl.java`（`transformPersistPatient`）、`FhirPersistanceServiceImpl.java`（送信の部分）、`fhir/service/PatientTransformServiceImpl.java`（grep のみ）、`config/AsyncConfig.java`、`config/AsyncExceptionHandler.java`
- 設定値: `common/util/DefaultConfigurationProperties.java`（読み込み順）、`liquibase/2.3.x.x/national_id_config.xml`、`liquibase/3.5.x.x/090-patient-search-trigram-indexes.xml`、`liquibase/3.5.x.x/111-menu-privilege-urls.xml`

### 読まなかった範囲

- `CreatePatientForm.tsx` の残り約 1800 行（住所階層、写真、身分証、連絡先、検証スキーマの中身）。国民識別番号の欄に画面側の形式チェックがあるかは見ていない。
- 写真・身分証の保存（`PatientPhotoService`、`PatientIdDocumentService`）。今回の5項目では空のはずとして追っていない。
- `PersonServiceImpl`、`PatientIdentityServiceImpl`、`BaseObjectServiceImpl`／`BaseDAOImpl` の中身（INSERT の実行部分）。監査のフラグが立っていることだけ確認。
- `observation_history` の検索、統合済み患者の処理（`getMergedPatientsIn`）の中身。
- ログイン処理、セッションのタイムアウト、役割とモジュールの対応表の中身。
- 外部検索（`PatientSearchLocalAndExternalWorker`）とクライアントレジストリ連携。既定で無効と読んだため。
- FHIR サーバ（HAPI）の中身、analyzer-bridge、autoheal、certs の各イメージ。
- 旧画面（JSP、`webapp/pages`）と、同じ WAR が `/OpenELIS-Global` にも配備されるという経路（`persistence.xml` のコメントにそう書いてある。persistence.xml:257-268）。
- liquibase のマイグレーション全体（必要な語を grep しただけ。`patient` 表にあとから足された制約やトリガーがあれば見落としている可能性がある）。
- `app` の `docs/`、`specs/`、`AGENTS.md`、`CLAUDE.md`、テストコード。指示としては扱わず、内容も根拠に使っていない。
