# 東京AIハッカソン 2026 一次審査の提出資料（作業中）

`一次審査_提出資料_6016ad5.pdf` は、[この実行の記録](../validation-clinic-demo-video-2026-11-07.md)の数字だけで作った A4・1ページの資料です。`run.json` は `tools/clinic-demo.mjs` がその実行で書いた記録です。

作り直すときは、このディレクトリで次を実行します（Python の python-docx と、Playwright のブラウザが必要です）。

```sh
python3 make_submission.py run.json <出力先> result.png
node render.cjs <出力先>
python3 make_docx.py <出力先> result.png
```

`render.cjs` は、1ページに収まらないと失敗します。DOCX は文章を直すための元データで、LibreOffice で PDF にすると2ページになるため、提出用の PDF は HTML から作ります。

## 名前の出し方

本人の指示（2026年11月7日）により、プロダクト名と氏名は冒頭の記載欄に1回ずつだけ置き、見出し・本文・図の説明では繰り返さない。大見出しは、解決する課題とできることにする。

## 提出前に残っていること

- デモ動画の共有URLを発行し、資料の「審査用の動作環境」の①に記入する。
- 既存の Google ドキュメントは旧版のまま。
- 利用者への聞き取りと試用、持続性の裏付けは集めていない。資料と動画の検証は、作成者による動作確認だけ。
