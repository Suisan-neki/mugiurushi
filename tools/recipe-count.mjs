// レシピのうち、対象アプリのHTMLやスキーマに依存する項目を数える。
// 出所の記録（recipe.browser.provenance）がある項目は「観測から作った項目」として分けて数える。
// 数は記述の量を示すだけで、試験の妥当性や網羅性は示さない。
import {readFileSync} from 'node:fs';

/** レシピ1件の、種類ごとの項目のIDを集める。 */
export function items(recipe) {
  const browser = recipe.browser || {};
  const steps = [...(browser.preparation || []), ...(browser.actions || [])];
  // 1つの手順が、操作の対象と表示の対象の2つを指す場合がある。
  const selectors = steps.flatMap(s => ['target', 'content'].filter(k => s[k]?.locator).map(k => ({id: s.id, key: `${s.id}.${k}`})));
  return {
    selectors,
    urls: [...(browser.responses || []), ...(browser.http_reads || [])].map(w => ({id: w.id, key: w.id})),
    database: (browser.database || []).map(p => ({id: p.id, key: p.id})),
    checks: (recipe.trial?.checks || []).map(c => ({id: c.id, key: c.id})),
  };
}

/** 種類ごとに、手で書いた項目と、出所の記録がある項目の数を返す。 */
export function count(recipe) {
  const provenance = recipe.browser?.provenance || {};
  const known = {selectors: provenance.steps || {}, urls: provenance.responses || {}, database: provenance.database || {}, checks: provenance.checks || {}};
  const handwritten = {}, derived = {};
  for (const [kind, list] of Object.entries(items(recipe))) {
    derived[kind] = list.filter(item => Object.hasOwn(known[kind], item.id)).length;
    handwritten[kind] = list.length - derived[kind];
  }
  const total = values => Object.values(values).reduce((a, b) => a + b, 0);
  return {handwritten: {...handwritten, total: total(handwritten)}, derived: {...derived, total: total(derived)}};
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const files = process.argv.slice(2);
  if (!files.length) { console.error('使い方: node tools/recipe-count.mjs <レシピのJSON>...'); process.exit(2); }
  console.log(JSON.stringify(files.map(file => ({file, ...count(JSON.parse(readFileSync(file, 'utf8')))})), null, 1));
}
