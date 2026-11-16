// 値を追跡した試行の候補（case draft の出力）を、レシピへ取り込む。対象ごとの処理は持たない。
// 取り込むのは、DBの読み出し先、応答の観測先、判定条件である。画面の操作と、値の割り当ての指定は変えない。

/** 候補が作る判定条件の種類。この種類のレシピの条件は、候補に置き換える。 */
const GENERATED = ['database_rows', 'browser_response', 'container_log'];

/**
 * @param {object} recipe 候補のもとになった試行で実行したレシピ。値の追跡と、手順ごとの値の指定を含む。
 * @param {object} draft `mugiurushi case draft` の出力。
 * @returns {object} 候補を取り込んだレシピ。`recipe` は変えない。
 */
export function adopt(recipe, draft) {
  const adopted = structuredClone(recipe), browser = adopted.browser;
  // 省略できる項目は、レシピに書かれていないことがある。
  browser.responses ||= []; browser.database ||= [];
  adopted.trial.checks ||= [];
  const provenance = {...(browser.provenance || {})};
  provenance.responses = {...(provenance.responses || {})};

  // 応答の観測先。同じ要求を指すものがレシピにあればそれを使い、なければ足す。
  const same = w => browser.responses.find(k => k.method === w.method && k.path === w.path);
  const target = Object.fromEntries(draft.responses.map(w => [w.id, same(w)?.id || w.id]));
  for (const w of draft.responses) if (!same(w)) { browser.responses.push(w); provenance.responses[w.id] = draft.provenance.responses[w.id]; }

  // 判定条件。候補が作る種類は候補に置き換え、ほかの種類（画面の表示、DBの行どうしの対応など）は残す。
  const kept = adopted.trial.checks.filter(c => !GENERATED.includes(c.method));
  adopted.trial.checks = [...kept, ...draft.checks.map(c => c.method === 'browser_response' ? {...c, target: target[c.target]} : c)];
  provenance.checks = draft.provenance.checks;

  // DBの読み出し先。候補に置き換えるが、残した条件が参照する読み出し先は残す。
  // 残さないと、DBの行どうしの対応の条件が、存在しない読み出し先を指す。
  const referenced = new Set(kept.flatMap(c => [c.target, c.relation?.left, c.relation?.right]).filter(Boolean));
  const needed = browser.database.filter(p => referenced.has(p.id));
  const taken = new Set(needed.map(p => p.id));
  const added = draft.database.filter(p => !taken.has(p.id));
  browser.database = [...needed, ...added];
  provenance.database = Object.fromEntries(added.map(p => [p.id, draft.provenance.database[p.id]]).filter(([, basis]) => basis));

  // 値の割り当て（track_values と value_per_step）は、候補のもとになった試行のままにする。
  // 手順ごとの値で作った候補は、読み出しと判定条件に手順ごとの値を持つ。割り当てを変えると、入力した値と一致しない。
  browser.provenance = provenance;
  return adopted;
}
