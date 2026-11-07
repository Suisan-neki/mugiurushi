"""東京AIハッカソン 一次審査の提出資料を、録画した実行の記録から作る。

使い方: python3 make_submission.py <clinic-demo.json> <出力ディレクトリ> <結果画面の切り抜きPNG>
数字・代表例・応答IDはすべて記録から読む。資料と動画を同じ実行にそろえるため。
"""
import html
import json
import sys
from pathlib import Path

record_path, out_dir, result_png = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
r = json.loads(record_path.read_text())
out_dir.mkdir(parents=True, exist_ok=True)

findings = r["scan"]["findings"]
testable = len(r["scan"]["testable"])
tests = len(r["plan"]["tests"])
executed = len(r["trials"])
ng = sum(1 for s in r["result_states"] if s == "ng")
pending = sum(1 for s in r["result_states"] if s == "pending")
rep = r["representative"]
trial = r["trials"][0]
status = {a["id"]: a["status"] for a in trial["aspects"]}
assert status.get(rep["id"]) == "contradicted", "代表例が不足として確かめられていない"
log_ng = any(l["check"] == "c3" and status.get(l["finding"]) == "contradicted" for l in trial["linked"])
log_text = "同じ試験で、操作の記録が残るかも確かめ、記録は残っていなかった。" if log_ng else ""
scan_call, plan_call = r["scan"]["model_call"], r["plan"]["model_call"]
commit = r["commit"][:7]
VIDEO = "共有URLは未発行（提出前に発行して記入する）"
REPO = "https://github.com/Suisan-neki/mugiurushi"
RECORD = "docs/validation-clinic-demo-video-2026-11-07.md"

numbers = (f"この実行では、AIが出した要件の候補{findings}件のうち、試験の観測に対応づけられたのは{testable}件。"
           f"代表例の試験{executed}件を実行し、そこで確かめた{ng}件はすべて期待と異なった（不足）。"
           f"残る{pending}件は試験を実行しておらず、合否に数えていない（未実行）。")

E = html.escape
CSS = """
@page{size:A4;margin:0}
*{box-sizing:border-box}
body{margin:0;font-family:'Noto Sans JP',sans-serif;color:#1A1A1A;font-size:8.4pt;line-height:1.45}
.page{width:210mm;height:297mm;padding:7mm 10mm 6mm;display:flex;flex-direction:column;gap:1.8mm}
.head{display:flex;justify-content:space-between;align-items:flex-end;border-bottom:2.4pt solid #0017C1;padding-bottom:1.6mm}
.head h1{margin:0;font-size:15pt;color:#00118F;line-height:1.2}
.head .sub{font-size:8.6pt;color:#333}
.head .who{text-align:right;font-size:7.6pt;color:#444;line-height:1.35}
.lead{background:#E8F1FE;border-left:3pt solid #0017C1;padding:1.6mm 3mm;font-size:9.2pt}
.lead b{color:#00118F}
.flow{display:grid;grid-template-columns:repeat(5,1fr);gap:2.4mm}
.step{border:1pt solid #0017C1;border-radius:2mm;padding:1.6mm 2mm;position:relative;background:#fff}
.step .n{display:inline-block;background:#0017C1;color:#fff;border-radius:1mm;padding:0 1.4mm;font-weight:700;font-size:8pt}
.step .t{font-weight:700;color:#00118F;font-size:8.8pt;margin-top:.6mm}
.step .who{font-size:7.6pt;color:#555}
.step .d{font-size:7.8pt;line-height:1.4;margin-top:.6mm}
.step:not(:last-child)::after{content:'▶';position:absolute;right:-2.6mm;top:40%;color:#0017C1;font-size:7pt}
.example{display:grid;grid-template-columns:1.25fr .75fr;gap:3mm;border:1pt solid #B3C7F5;border-radius:2mm;padding:2mm 2.6mm}
.example h2,.sec h2{margin:0 0 1mm;font-size:10pt;color:#00118F}
.example dl{margin:0;display:grid;grid-template-columns:13mm 1fr;row-gap:.8mm;column-gap:2mm}
.example dt{font-weight:700;color:#0017C1}
.example dd{margin:0}
.ng{color:#C42B1C;font-weight:700}
.shot img{max-height:40mm;max-width:100%;border:.6pt solid #999;display:block}
.shot .cap{font-size:7.4pt;color:#555;margin-top:.8mm}
.numbers{margin-top:1.4mm;font-size:8pt;background:#F5F5F5;padding:1.4mm 2.4mm;border-radius:1.4mm}
.cols{display:grid;grid-template-columns:1fr 1fr;gap:2.4mm 4mm}
.sec{border-top:1pt solid #B3C7F5;padding-top:1.2mm}
.sec h2 .no{display:inline-block;background:#00118F;color:#fff;border-radius:1mm;padding:0 1.4mm;margin-right:1.4mm;font-size:8.6pt}
.sec p{margin:.4mm 0}
.sec b{color:#00118F}
.env{border:1.2pt solid #0017C1;border-radius:2mm;padding:1.6mm 2.6mm;font-size:8.4pt}
.env b{color:#00118F}
.col{display:flex;flex-direction:column;gap:1.8mm}
.foot{font-size:7.2pt;color:#555;margin-top:auto}
"""

def sec(no, title, items):
    body = "".join(f"<p><b>{E(k)}</b>　{E(v)}</p>" for k, v in items)
    return f'<div class="sec"><h2><span class="no">{no}</span>{E(title)}</h2>{body}</div>'

S = {
    "lead": [("誰が", "医療機関向けのシステムに新しく参入する開発者。"),
             ("何に困るか", "医療機関は採用時に、ガイドラインにどこまで対応しているかの説明を事業者に求める（サービス仕様適合開示書など）。新規参入者は、自分のコードのどこがどの要件に関わるかを示しにくい。"),
             ("これで確かめられること", "公開ガイドラインから出典つきで要件の候補を出し、開発者が選んだ要件を、対象を実際に動かして、選んだ観測の範囲で確かめる。適合・準拠の判定はしない。")],
    "flow": [("コードと説明", "開発者", "対象のコードとシステムの説明を入力する"),
             ("出典つきの候補", "AI", "取り込んだ公開文書18件から、関わりうる要件を出典つきで挙げる"),
             ("試験範囲を選ぶ", "開発者", "試験の案を読んで了承し、候補ごとに判定の条件を選ぶ"),
             ("実行して確かめる", "実行・観測基盤（Rust）", "対象を実際に起動し、出力・保存先・読み出しを観測して判定する"),
             ("修正の依頼", "開発者が手元のAIへ", "出典・判定の条件・結果・関係するコードを1つの依頼文にまとめる")],
    "c1": [("当事者と場面", "医療機関向けシステムを新たに作る小規模事業者の開発者が、採用時にガイドライン対応の説明を求められる場面。"),
           ("現在の対処と限界", "ガイドラインを自分で読み、チェックリストに自己申告するか、専門家に依頼する。自己申告だけでは、動かして確かめた記録は残らない。"),
           ("一次情報", "作成者自身が開発者として、要件の把握と検証の負担を課題とした（2026年10月25日の発言をリポジトリに記録）。当事者への聞き取りの記録はこの資料には含めていない。"),
           ("実際に扱う負担", "公開文書の要件と、対象のアプリで確かめる操作・期待する結果・証拠を、開発者が自分で対応づける必要がある。")],
    "c2": [("AIが取り除く制約", "専門知識と時間。公開文書から関わる要件を出典（文書・ページ・SHA-256）つきで挙げ、「何をして何を観測すれば確かめられるか」という試験の案に翻訳する。AIが無いと、これを人が要件ごとに行う。"),
           ("任せる／任せない", "任せる：候補、質問、試験の案、修正依頼の文。任せない：案の了承と判定の条件（開発者）、実行と判定（基盤）。合否の誤りは誤った説明につながり、再現性が要るため。")],
    "c3": [("影響が届く人", "直接：医療情報システムを作る事業者の開発者（特に新規参入者）。間接：採用する医療機関の担当者と、記録される患者。"),
           ("影響を与える仕組み", "医療機関と事業者の間の、ガイドラインに基づく安全管理の説明と役割分担。新規参入者が根拠を持って説明できれば、医療機関の選択肢が広がる。")],
    "c4": [("回り続ける仕組み", f"公開リポジトリのオープンソースとして、作成者が個人で開発している。維持に要るのは、ガイドライン改訂時の文書の取り込み直し（取得日とSHA-256の記録は実装済み）と、利用者が自分のキーで払うAIの利用料（この実行では入力{scan_call['input_tokens'] + plan_call['input_tokens']:,}・出力{scan_call['output_tokens'] + plan_call['output_tokens']:,}トークン）。"),
           ("裏付け", "関係者の行動や約束による裏付けは、この資料には含めていない。")],
    "c5": [("利用者と目的", "医療システムの開発者が導入前に、選んだ要件の不足を確かめて直すため。"),
           ("できていること", "上の流れを、実際のAIと架空の診療所アプリの実際の起動で最後まで通した（作成者による実装検証）。"),
           ("できていないこと", "試験の案から試験の設定を自動で作ること。修正した版での再試験。"),
           ("利用者による試用", "記録はこの資料には含めていない。上の検証はすべて作成者による。")],
}

flow_html = "".join(f'<div class="step"><span class="n">{i + 1}</span><div class="t">{E(t)}</div><div class="who">{E(w)}</div><div class="d">{E(d)}</div></div>' for i, (t, w, d) in enumerate(S["flow"]))
lead_html = "<br>".join(f"<b>{E(k)}：</b>{E(v)}" for k, v in S["lead"])
example = f"""
<div class="example">
 <div>
  <h2>代表例：担当外の職員に、患者のタスクが見えないか</h2>
  <dl>
   <dt>出典</dt><dd>{E(rep['source'].split('（')[0])}「{E(rep['requirement'])}」</dd>
   <dt>操作</dt><dd>職員nurse1が患者P001のタスクを追加し、別の職員nurse2で一覧を読み直す。</dd>
   <dt>期待</dt><dd>nurse2の一覧に、そのタスクが出ない。</dd>
   <dt>結果</dt><dd><span class="ng">出た（期待と異なる）</span>。{E(log_text)}</dd>
   <dt>次</dt><dd>出典の抜粋・判定の条件・結果・関係するコードを、修正の依頼文にまとめた。修正と再試験はまだ行っていない。</dd>
  </dl>
  <div class="numbers">{E(numbers)}</div>
 </div>
 <div class="shot"><img src="{result_png.name}"><div class="cap">結果の画面（同じ実行）。確かめた候補は「試験で不足を確認」、試験していない候補は「未実行」と表示する。</div></div>
</div>"""

page = f"""<!doctype html><html lang="ja"><head><meta charset="utf-8"><title>一次審査 提出資料</title><style>{CSS}</style></head><body><div class="page">
<div class="head"><div><h1>ガイドラインの要件を、実際に動かす試験で確かめる</h1><div class="sub">医療システムに新しく参入する開発者が、選んだ要件の不足を見つけ、直す依頼までつなげる</div></div><div class="who">東京AIハッカソン 2026 一次審査<br>プロダクト名：mugiurushi（麦漆）<br>名前：山下真凜</div></div>
<div class="lead">{lead_html}</div>
<div class="flow">{flow_html}</div>
{example}
<div class="cols">
<div class="col">{sec(1, '課題', S['c1'])}{sec(5, '実装', S['c5'])}</div>
<div class="col">{sec(2, '技術の役割', S['c2'])}{sec(3, '社会的インパクト', S['c3'])}{sec(4, '持続性', S['c4'])}</div>
</div>
<div class="env"><b>審査用の動作環境</b>　<br>①デモ動画（約3分・無編集、操作は自動、AIと試験の実行は実物）：{E(VIDEO)}<br>②この実行の記録：{E(REPO)}/blob/a6361bc/{RECORD}<br>③手元で動かす手順（任意）：{E(REPO)}/blob/b6302a8/docs/judging.md</div>
<div class="foot">数字はすべて動画と同じ1回の実行（{E(r['started_at'][:10])}、架空のアプリのコミット {commit}）。AIの応答は実行ごとに変わる。対象は架空のアプリと架空のデータだけ。</div>
</div></body></html>"""
(out_dir / "submission.html").write_text(page)
(out_dir / "content.json").write_text(json.dumps({"S": S, "numbers": numbers, "rep": rep, "log_text": log_text, "video": VIDEO, "repo": REPO, "record": RECORD, "commit": commit, "scan_call": scan_call, "plan_call": plan_call, "started": r["started_at"]}, ensure_ascii=False, indent=1))
print("ok", findings, testable, tests, executed, ng, pending)
