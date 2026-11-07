// ピッチ用動画の文字の画面と、デモ場面の背景（右の説明パネル・上の段階表示）を 1920x1080 の PNG にする。
const { chromium } = require('playwright');
const fs = require('fs');
const path = require('path');
const out = process.argv[2];
const B = '#0017C1', D = '#00118F', L = '#E8F1FE';
const base = `*{box-sizing:border-box}body{margin:0;width:1920px;height:1080px;font-family:'Noto Sans JP',sans-serif;overflow:hidden}`;
const badge = `<div style="position:absolute;left:0;right:0;bottom:0;height:52px;background:#0B1B4D;color:#C9D6FF;font-size:20px;display:flex;align-items:center;padding:0 40px;gap:24px"><b style="color:#fff">mugiurushi</b><span>実際の画面（架空のアプリ・実行 6016ad5）</span><span style="margin-left:auto">編集版（場面の抜粋・早送り・切り抜きと拡大・説明カードと字幕）／無編集版あり</span></div>`;
const STEPS = ['コードと説明', '出典つきの候補', '試験範囲を選ぶ', '実行して確かめる', '修正の依頼'];
const steps = cur => `<div style="position:absolute;left:40px;right:40px;top:28px;display:flex;gap:12px">${STEPS.map((s, i) => `<div style="flex:1;padding:12px 16px;border-radius:10px;font-size:22px;font-weight:700;${i + 1 === cur ? `background:${B};color:#fff` : i + 1 < cur ? `background:${L};color:${D}` : 'background:#F2F2F2;color:#888'}">${i + 1}　${s}</div>`).join('')}</div>`;
const esc = t => t.replace(/[&<>]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[c]);

const cards = {
  hook: `<body style="background:${D};color:#fff;display:flex;flex-direction:column;justify-content:center;padding:0 140px">
    <div style="font-size:30px;opacity:.8;margin-bottom:28px">医療機関にシステムを採用してもらうとき、事業者が聞かれる</div>
    <div style="font-size:76px;font-weight:700;line-height:1.35">「このシステム、ガイドラインに<br>どこまで対応していますか？」</div>
    <div style="font-size:26px;opacity:.75;margin-top:36px">例：サービス仕様適合開示書での説明</div></body>`,
  problem: `<body style="background:#fff;color:#1A1A1A;display:flex;flex-direction:column;justify-content:center;padding:0 140px">
    <div style="font-size:64px;font-weight:700;color:${D};line-height:1.35">新しく参入した開発者は、<br>自分のコードで、どう示せばいい？</div>
    <div style="font-size:34px;line-height:1.7;margin-top:44px;border-left:8px solid ${B};padding-left:28px">公開文書の要件と、自分のアプリで確かめる<br><b>操作・期待する結果・証拠</b>を、自分で対応づける必要がある。</div></body>`,
  solution: `<body style="background:${L};color:#1A1A1A;display:flex;flex-direction:column;justify-content:center;padding:0 140px">
    <div style="font-size:96px;font-weight:700;color:${D}">mugiurushi</div>
    <div style="font-size:46px;font-weight:700;margin-top:10px">ガイドラインの要件を、<span style="color:${B}">実際に動かす試験</span>で確かめる。</div>
    <div style="display:flex;gap:16px;margin-top:60px">${STEPS.map((s, i) => `<div style="flex:1;background:#fff;border:3px solid ${B};border-radius:14px;padding:22px 18px;font-size:28px;font-weight:700;color:${D}"><span style="background:${B};color:#fff;border-radius:6px;padding:0 10px;margin-right:8px">${i + 1}</span>${s}</div>`).join('')}</div>
    <div style="font-size:26px;color:#444;margin-top:30px">候補を出すのはAI。試験の範囲を決めるのは開発者。合否は、実際に動かした結果だけで決める。</div></body>`,
  reveal: `<body style="background:#fff;color:#1A1A1A;display:flex;flex-direction:column;justify-content:center;padding:0 140px">
    <div style="font-size:32px;color:#C42B1C;font-weight:700">実際に動かして、見つかったこと</div>
    <div style="font-size:72px;font-weight:700;line-height:1.35;margin-top:20px">担当外の職員の一覧に、<br>患者のタスクが<span style="color:#C42B1C">出ていた。</span></div>
    <div style="font-size:34px;line-height:1.7;margin-top:36px">同じ試験で、操作の記録（誰が追加したか）も<span style="color:#C42B1C;font-weight:700">残っていなかった。</span></div>
    <div style="font-size:24px;color:#555;margin-top:30px">期待：nurse2 の一覧に、nurse1 が追加した患者 P001 のタスクは出ない ／ 出典：OWASP Top 10:2025 A01（アクセス制御の不備）<br>選んだ観測の範囲で見つかった不足。この要件のほかの部分は確かめていない。</div></body>`,
  numbers: `<body style="background:${D};color:#fff;display:flex;flex-direction:column;justify-content:center;padding:0 110px">
    <div style="font-size:34px;opacity:.85">この1回の実行で確かめたこと</div>
    <div style="display:flex;align-items:center;gap:30px;margin-top:40px;font-weight:700">
      <div style="width:330px"><div style="font-size:140px;line-height:1">8<span style="font-size:46px">件</span></div><div style="font-size:30px;margin-top:12px">AIが出した要件の候補</div></div>
      <div style="font-size:60px;opacity:.6">→</div>
      <div style="width:360px"><div style="font-size:140px;line-height:1">4<span style="font-size:46px">件</span></div><div style="font-size:30px;margin-top:12px">試験の観測に対応づけ</div></div>
      <div style="font-size:90px;opacity:.8">＝</div>
      <div style="flex:1;border:3px solid rgba(255,255,255,.5);border-radius:18px;padding:22px 30px;display:flex;flex-direction:column;gap:16px">
        <div style="display:flex;align-items:baseline;gap:20px"><span style="font-size:100px;line-height:1;color:#FF8A80">3<span style="font-size:40px">件</span></span><span style="font-size:32px">動かして不足を確認</span></div>
        <div style="font-size:36px;opacity:.7;margin-left:6px">＋</div>
        <div style="display:flex;align-items:baseline;gap:20px"><span style="font-size:100px;line-height:1;color:#C9D6FF">1<span style="font-size:40px">件</span></span><span style="font-size:32px">未実行（試験していない）</span></div>
      </div>
    </div>
    <div style="font-size:28px;line-height:1.6;margin-top:44px;border-top:2px solid rgba(255,255,255,.4);padding-top:22px">不足を確認した3件も、選んだ観測の範囲で確かめただけで、各要件には確かめていない部分が残る。<br>試験していない1件は、合格にも不合格にも数えない。</div></body>`,
  close: `<body style="background:#fff;color:#1A1A1A;display:flex;flex-direction:column;justify-content:center;padding:0 140px">
    <div style="font-size:70px;font-weight:700;color:${D};line-height:1.35">推測ではなく、<br>動かした証拠で説明する。</div>
    <div style="font-size:34px;margin-top:40px">不足が見つかったら、出典・判定の条件・結果・関係するコードをまとめた修正の依頼を、手元のAIに渡せる。</div>
    <div style="font-size:24px;color:#555;margin-top:40px">mugiurushi（麦漆）／ 山下真凜 ／ github.com/Suisan-neki/mugiurushi<br>対象は架空のアプリと架空のデータ。修正と、修正後の再試験は、この動画では行っていない。</div></body>`,
};
const scenes = JSON.parse(fs.readFileSync(path.join(out, 'scenes.json'), 'utf8'));
(async () => {
  const b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 1920, height: 1080 } });
  for (const [name, body] of Object.entries(cards)) {
    await p.setContent(`<!doctype html><meta charset="utf-8"><style>${base}</style>${body}`);
    await p.evaluate(() => document.fonts.ready);
    await p.screenshot({ path: path.join(out, `card-${name}.png`) });
  }
  for (const s of scenes) {
    await p.setContent(`<!doctype html><meta charset="utf-8"><style>${base}</style><body style="background:#fff;position:relative">${steps(s.step)}
      <div style="position:absolute;left:40px;top:118px;width:1280px;height:720px;border:3px solid ${B};border-radius:6px;background:#eee"></div>
      <div style="position:absolute;left:1360px;top:118px;width:520px">
        <div style="font-size:26px;font-weight:700;color:${B}">${s.step}　${esc(STEPS[s.step - 1])}</div>
        <div style="font-size:44px;font-weight:700;color:${D};line-height:1.35;margin-top:14px">${esc(s.title)}</div>
        ${s.lines.map(l => `<div style="font-size:28px;line-height:1.6;margin-top:22px">${esc(l)}</div>`).join('')}
        ${s.note ? `<div style="font-size:22px;color:#555;line-height:1.5;margin-top:26px;border-top:1px solid #ccc;padding-top:14px">${esc(s.note)}</div>` : ''}
      </div>${badge}</body>`);
    await p.evaluate(() => document.fonts.ready);
    await p.screenshot({ path: path.join(out, `bg-${s.id}.png`) });
  }
  await b.close();
})();
