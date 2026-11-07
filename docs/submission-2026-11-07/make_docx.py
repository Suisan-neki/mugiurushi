"""submission.html と同じ内容を、編集できる DOCX にする（content.json から作る）。"""
import json
import sys
from pathlib import Path
from docx import Document
from docx.enum.table import WD_TABLE_ALIGNMENT
from docx.oxml import OxmlElement
from docx.oxml.ns import qn
from docx.shared import Mm, Pt, RGBColor

d_dir = Path(sys.argv[1]); img = Path(sys.argv[2])
c = json.loads((d_dir / "content.json").read_text())
S = c["S"]
FONT, BLUE, DARK = "Noto Sans JP", RGBColor(0x00, 0x11, 0x8F), RGBColor(0x1A, 0x1A, 0x1A)
doc = Document()
sec = doc.sections[0]
sec.page_width, sec.page_height = Mm(210), Mm(297)
sec.left_margin = sec.right_margin = Mm(12); sec.top_margin = Mm(9); sec.bottom_margin = Mm(8)
st = doc.styles["Normal"]; st.font.name = FONT; st.font.size = Pt(8.6)
st.element.rPr.rFonts.set(qn("w:eastAsia"), FONT)
st.paragraph_format.space_after = Pt(1); st.paragraph_format.line_spacing = 1.15

def run(p, t, bold=False, color=None, size=None):
    r = p.add_run(t); r.bold = bold; r.font.name = FONT; r._element.rPr.rFonts.set(qn("w:eastAsia"), FONT)
    if color is not None: r.font.color.rgb = color
    if size: r.font.size = Pt(size)
    return r

def shade(cell, fill):
    pr = cell._element.get_or_add_tcPr(); sh = OxmlElement("w:shd"); sh.set(qn("w:val"), "clear"); sh.set(qn("w:fill"), fill); pr.append(sh)

def heading(text, no=None):
    p = doc.add_paragraph(); p.paragraph_format.space_before = Pt(4)
    if no: run(p, f"{no} ", True, BLUE, 10)
    run(p, text, True, BLUE, 10)

p = doc.add_paragraph(); run(p, "東京AIハッカソン 2026 一次審査　｜　プロダクト名：mugiurushi（麦漆）　｜　名前：山下真凜", size=8)
p = doc.add_paragraph(); run(p, "ガイドラインの要件を、実際に動かす試験で確かめる", True, BLUE, 15)
p = doc.add_paragraph(); run(p, "医療システムに新しく参入する開発者が、選んだ要件の不足を見つけ、直す依頼までつなげる", size=8.4)

t = doc.add_table(rows=1, cols=1); cell = t.rows[0].cells[0]; shade(cell, "E8F1FE"); cell.text = ""
for i, (k, v) in enumerate(S["lead"]):
    q = cell.paragraphs[0] if i == 0 else cell.add_paragraph()
    run(q, k + "：", True, BLUE); run(q, v)

doc.add_paragraph()
t = doc.add_table(rows=1, cols=5); t.style = "Table Grid"; t.alignment = WD_TABLE_ALIGNMENT.CENTER
for i, (title, who, desc) in enumerate(S["flow"]):
    cell = t.rows[0].cells[i]; cell.text = ""
    q = cell.paragraphs[0]; run(q, f"{i + 1} {title}", True, BLUE, 8.6)
    q = cell.add_paragraph(); run(q, who, size=7.6, color=RGBColor(0x55, 0x55, 0x55))
    q = cell.add_paragraph(); run(q, desc, size=7.8)

heading("代表例：担当外の職員に、患者のタスクが見えないか")
rep = c["rep"]
rows = [("出典", f"{rep['source'].split('（')[0]}「{rep['requirement']}」"),
        ("操作", "職員nurse1が患者P001のタスクを追加し、別の職員nurse2で一覧を読み直す。"),
        ("期待", "nurse2の一覧に、そのタスクが出ない。"),
        ("結果", "出た（期待と異なる）。" + c["log_text"]),
        ("次", "出典の抜粋・判定の条件・結果・関係するコードを、修正の依頼文にまとめた。修正と再試験はまだ行っていない。")]
t = doc.add_table(rows=1, cols=2); left, right = t.rows[0].cells; left.text = ""
for i, (k, v) in enumerate(rows):
    q = left.paragraphs[0] if i == 0 else left.add_paragraph()
    run(q, k + "　", True, BLUE); run(q, v, k == "結果", RGBColor(0xC4, 0x2B, 0x1C) if k == "結果" else None)
q = left.add_paragraph(); run(q, c["numbers"], size=8)
right.text = ""; right.paragraphs[0].add_run().add_picture(str(img), width=Mm(62))
q = right.add_paragraph(); run(q, "結果の画面（同じ実行）。確かめた候補は「試験で不足を確認」、試験していない候補は「未実行」と表示する。", size=7.4)

for no, title, key in [(1, "課題", "c1"), (2, "技術の役割", "c2"), (3, "社会的インパクト", "c3"), (4, "持続性", "c4"), (5, "実装", "c5")]:
    heading(title, no)
    for k, v in S[key]:
        q = doc.add_paragraph(); run(q, k + "　", True, BLUE); run(q, v)

heading("審査用の動作環境")
for line in [f"①デモ動画（約3分・無編集。操作はスクリプトによる自動操作、AIと試験の実行は実物）：{c['video']}",
             f"②この実行の記録（応答ID・コミット）：{c['repo']}/blob/a6361bc/{c['record']}",
             f"③手元で動かす手順（任意）：{c['repo']}/blob/b6302a8/docs/judging.md"]:
    q = doc.add_paragraph(); run(q, line)
q = doc.add_paragraph(); run(q, f"数字はすべて動画と同じ1回の実行（{c['started'][:10]}、架空のアプリのコミット {c['commit']}）。AIの応答は実行ごとに変わる。対象は架空のアプリと架空のデータだけ。", size=7.2)
doc.save(d_dir / "submission.docx")
print("docx ok")
