# ピッチ用の編集版動画

`source-narrated-6016ad5.mp4` は、`tools/clinic-demo.mjs` で撮った無編集の録画です（[実行の記録](../../validation-clinic-demo-video-2026-11-07.md)）。編集版は、この録画から次の編集をして作ります。

- 場面の抜粋（`scenes.json` の `src` の秒数）
- AIの応答待ちの早送り（`speed`。画面に早送りと実際の長さを表示する）
- 切り抜きと拡大（`crop`）
- 説明のカード（掴み・困り事・解決・発見・集計・締め）と、各場面の右側の説明

作り直すときは、このディレクトリで次を実行します（Playwright のブラウザ、ffmpeg、bc、Noto Sans JP のフォントが必要です）。

```sh
node cards.cjs .
./build.sh "$PWD/source-narrated-6016ad5.mp4" "$PWD/ピッチ動画_6016ad5_v3.mp4"
```

数字と代表例は、上の記録と同じ1回の実行のものです。「不足を確認」は選んだ観測の範囲での結果で、各要件には確かめていない部分が残ります。修正と、修正後の再試験は行っていません。
