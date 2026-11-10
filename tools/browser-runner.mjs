// 基盤に組み込むブラウザ操作。対象リポジトリのスクリプトは読み込まない。
import fs from 'node:fs/promises';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const request=JSON.parse(await fs.readFile(process.argv[2],'utf8'));
const dir=path.dirname(process.argv[2]), spec=request.spec;
const report={schema_version:1,run_id:request.run_id,version:'',prepared:false,completed:false,steps:[],responses:[],blocked_requests:0,response_limit:false,exchanges:[],exchange_limit:false,detail:'',launch_args:['--use-gl=angle','--use-angle=swiftshader','--enable-unsafe-swiftshader']};
if(process.env.EEL_BROWSER_EXTRA_ARGS){const args=JSON.parse(process.env.EEL_BROWSER_EXTRA_ARGS);if(!Array.isArray(args)||args.length>16||args.some(s=>typeof s!=='string'||s.includes('\0')||['--no-sandbox','--disable-setuid-sandbox','--disable-web-security'].includes(s.split('=')[0])))throw Error('ブラウザーの追加引数が不正');report.launch_args.push(...args);}
let browser, page, secrets={}, masks=[], requests=new Map(), active=null, pending=new Set(), deadline=Date.now()+request.timeout_ms;
// 停止要求は操作待ちの間にもブラウザを閉じ、Playwrightが起動した子を回収する。
const closeBrowser=async()=>{if(browser)await browser.close().catch(()=>{});};
process.once('SIGTERM',()=>{void closeBrowser();});
const stopPoll=setInterval(()=>{void fs.stat(path.join(dir,'stop')).then(closeBrowser,()=>{});},50);stopPoll.unref();
const redact=text=>{let s=String(text);for(const value of Object.values(secrets).filter(v=>typeof v==='string'&&v.length)){s=s.split(value).join('[秘匿]');}return s.slice(0,8192);};
const replace=s=>s.replaceAll('{{test_value}}',request.test_value);
// 値の追跡。手順ごとの試験値が指定されていれば、その手順の入力だけに使う。
const stepValues=request.step_values||{},tracked=Array.isArray(request.tracked)?request.tracked.filter(t=>typeof t?.slot==='string'&&typeof t?.value==='string'&&t.value.length).slice(0,128):[];
const replaceFor=(step,s)=>s.replaceAll('{{test_value}}',typeof stepValues[step.id]==='string'?stepValues[step.id]:request.test_value);
// 文字列に含まれていた追跡する値の、出所の名前だけを返す。値そのものは記録に入れない。
const slotsIn=texts=>tracked.filter(t=>texts.some(text=>typeof text==='string'&&text.includes(t.value))).map(t=>t.slot);
const decoded=text=>{try{return decodeURIComponent(text.replace(/\+/g,' '));}catch{return '';}};
async function visibleSlots(){
 if(!tracked.length)return [];
 const seen=new Set();
 try{
  await Promise.race([(async()=>{
   for(const frame of page.frames()){
    let origin='';try{origin=new URL(frame.url()).origin;}catch{}
    if(!request.origins.includes(origin))continue;
    // 表示されている文字列だけを調べる。入力欄の中の値は innerText に含まれない。
    const found=await frame.evaluate(values=>{const text=document.body?document.body.innerText:'';return values.map(v=>text.includes(v));},tracked.map(t=>t.value));
    found.forEach((hit,i)=>{if(hit)seen.add(tracked[i].slot);});
   }
  })(),new Promise((_,reject)=>setTimeout(()=>reject(Error('timeout')),Math.max(1,Math.min(2000,deadline-Date.now()))))]);
 }catch{}
 return [...seen];
}
// 画面の構造とHTTPのやり取りの記録。入力欄の値・Cookieの値・本文は取らない。
const HEADER_NAMES=['content-security-policy','strict-transport-security','x-frame-options','x-content-type-options','referrer-policy','cache-control','content-type'];
const LIMITS={screens:96,frames:32,fields:300,controls:200,links:200,headings:50,text:256,exchanges:500,exchangeBytes:512*1024,header:512,bytes:4*1024*1024};
const screens=[];let exchangeBytes=0,screenBytes=0,screensTruncated=false,lastDocument=null,lastScreen=null;
const redactAll=value=>typeof value==='string'?redact(value):Array.isArray(value)?value.map(redactAll):value&&typeof value==='object'?Object.fromEntries(Object.entries(value).map(([k,v])=>[k,redactAll(v)])):value;
async function headersOf(response){
 // headers() はSet-Cookieを含まないため、全ヘッダーとSet-Cookieの各値を別に取る。
 const all=await response.allHeaders().catch(()=>response.headers()),headers={};
 for(const name of HEADER_NAMES)if(typeof all[name]==='string')headers[name]=redact(all[name]).slice(0,LIMITS.header);
 const cookies=(await response.headerValues('set-cookie').catch(()=>[])).slice(0,32).map(line=>{
  const parts=line.split(';').map(x=>x.trim()),flag=n=>parts.slice(1).some(x=>x.toLowerCase()===n),value=n=>parts.slice(1).find(x=>x.toLowerCase().startsWith(n+'='))?.slice(n.length+1)??null;
  return {name:parts[0].split('=')[0].slice(0,LIMITS.text),secure:flag('secure'),http_only:flag('httponly'),same_site:value('samesite')?.slice(0,16)??null,path:value('path')?.slice(0,LIMITS.text)??null};
 });
 return {headers,cookies};
}
// ページの中で実行する。DOMの属性だけを読み、valueは読まない。
function collectFrame(limits){
 const cut=s=>String(s??'').replace(/\s+/g,' ').trim().slice(0,limits.text),attr=(e,n)=>e.hasAttribute(n)?cut(e.getAttribute(n)):null;
 const labelOf=e=>{
  if(e.id){const l=[...document.querySelectorAll('label')].find(x=>x.htmlFor===e.id);if(l)return cut(l.innerText);}
  const parent=e.closest('label');if(parent)return cut(parent.innerText);
  if(e.hasAttribute('aria-label'))return cut(e.getAttribute('aria-label'));
  const by=(e.getAttribute('aria-labelledby')||'').split(/\s+/).map(id=>document.getElementById(id)).filter(Boolean);
  return by.length?cut(by.map(x=>x.innerText).join(' ')):null;
 };
 const pathOf=value=>{try{const u=new URL(value,location.href);return u.origin===location.origin?cut(u.pathname):null;}catch{return null;}};
 const take=(selector,max)=>{const all=[...document.querySelectorAll(selector)];return [all.slice(0,max),all.length>max];};
 const [fieldNodes,moreFields]=take('input,select,textarea',limits.fields),[controlNodes,moreControls]=take('button,input[type=submit],input[type=button],input[type=reset]',limits.controls);
 const [linkNodes,moreLinks]=take('a[href]',limits.links*4),[headingNodes,moreHeadings]=take('h1,h2,h3',limits.headings);
 const fields=fieldNodes.map(e=>({tag:e.tagName.toLowerCase(),input_type:e.tagName==='INPUT'?cut(e.type):null,name:attr(e,'name'),id:attr(e,'id'),label:labelOf(e),required:!!e.required,readonly:!!e.readOnly,disabled:!!e.disabled,autocomplete:attr(e,'autocomplete'),maxlength:e.hasAttribute('maxlength')&&/^\d{1,9}$/.test(e.getAttribute('maxlength'))?Number(e.getAttribute('maxlength')):null,form_action:e.form&&e.form.hasAttribute('action')?pathOf(e.form.getAttribute('action')):null,form_method:e.form?cut(e.form.method):null}));
 const controls=controlNodes.map(e=>({tag:e.tagName.toLowerCase(),input_type:e.tagName==='INPUT'?cut(e.type):attr(e,'type'),text:cut(e.tagName==='INPUT'?e.getAttribute('value')||e.getAttribute('aria-label'):e.innerText||e.getAttribute('aria-label'))}));
 const links=[...new Set(linkNodes.map(e=>pathOf(e.getAttribute('href'))).filter(Boolean))];
 return {path:cut(location.pathname),title:cut(document.title),headings:headingNodes.map(e=>cut(e.innerText)).filter(Boolean),fields,controls,links:links.slice(0,limits.links),truncated:moreFields||moreControls||moreLinks||moreHeadings||links.length>limits.links};
}
async function frameChain(frame){
 const chain=[];
 for(let f=frame;f.parentFrame();f=f.parentFrame()){
  const element=await f.frameElement();
  chain.unshift(await element.evaluate(e=>{const name=e.getAttribute('name'),id=e.getAttribute('id');let src='';try{src=new URL(e.getAttribute('src')||'',location.href).pathname;}catch{}return (name?`name=${name}`:id?`id=${id}`:`src=${src}`).slice(0,256);}));
 }
 return chain;
}
// 手順が完了した時点の画面の構造を残す。取れなかった場合も、手順の結果は変えない。
async function captureScreen(step,record,phase){
 if(screens.length>=LIMITS.screens||screenBytes>=LIMITS.bytes){screensTruncated=true;return;}
 const frames=[];let truncated=false;
 try{
  const within=ms=>new Promise((_,reject)=>setTimeout(()=>reject(Error('画面の構造の取得が時間内に終わらなかった')),ms));
  await Promise.race([(async()=>{
   for(const frame of page.frames()){
    let origin='';try{origin=new URL(frame.url()).origin;}catch{}
    if(!request.origins.includes(origin))continue;
    if(frames.length>=LIMITS.frames){truncated=true;break;}
    const content=await frame.evaluate(collectFrame,LIMITS);
    frames.push({chain:await frameChain(frame),path:content.path,title:content.title,headings:content.headings,fields:content.fields,controls:content.controls,links:content.links,truncated:content.truncated});
   }
  })(),within(Math.max(1,Math.min(3000,deadline-Date.now())))]);
 }catch(error){record.detail+='。画面の構造は未取得: '+redact(error.message);return;}
 const content=redactAll({path:frames[0]?.path??'',title:frames[0]?.title??'',frames:frames.map(({title,...rest})=>rest),document:lastDocument,truncated:truncated||frames.some(f=>f.truncated)});
 for(const f of content.frames)delete f.truncated;
 const text=JSON.stringify(content);
 if(lastScreen&&lastScreen.text===text){screens.push({step:step.id,phase,same_as:lastScreen.key,content:null});return;}
 if(screenBytes+text.length>LIMITS.bytes){screensTruncated=true;return;}
 screenBytes+=text.length;lastScreen={text,key:`${phase}:${step.id}`};screens.push({step:step.id,phase,same_as:null,content});
}
const save=async(name,data)=>{const temp=path.join(dir,name+'.tmp');await fs.writeFile(temp,JSON.stringify(data),{mode:0o600,flag:'wx'});await fs.rename(temp,path.join(dir,name));};
function target(t){let scope=page;for(const frame of t.frames||[])scope=scope.frameLocator(frame);const l=t.locator;return l.by==='label'?scope.getByLabel(l.text,{exact:true}):l.by==='role'?scope.getByRole(l.role,{name:l.name,exact:true}):scope.locator(l.selector);}
function remaining(){const n=deadline-Date.now();if(n<=0)throw Error('ブラウザ試験が待機上限に達した');return Math.min(n,15000);}
async function dismiss(step,record){
 // Bootstrap等の表示アニメーション中には閉じる要求が反映されない場合がある。
 // 再試行するのは宣言された準備用の閉じる操作だけ。保存等のclickには適用しない。
 const content=target(step.content),until=Date.now()+remaining();
 const time=()=>{const n=Math.min(remaining(),until-Date.now());if(n<=0)throw Error('案内を閉じる操作が待機上限に達した');return n;};
 await content.waitFor({state:'visible',timeout:time()});
 for(let attempt=1;attempt<=3;attempt++){
  if(attempt>1&&!await content.isVisible()){record.detail+='。案内の非表示を確認した';return;}
  record.detail=`案内を閉じるクリックの試行${attempt}回目`;
  await target(step.target).click({timeout:time()});
  record.detail=`案内を閉じるクリックを${attempt}回実行した`;
  try{await content.waitFor({state:'hidden',timeout:Math.min(750,time())});record.detail+='。案内の非表示を確認した';return;}
  catch(error){if(error.name!=='TimeoutError'||attempt===3)throw error;}
 }
}
async function screenshot(step,r,timeout,failed=false){
 if(step.action==='fill'&&step.value.source==='secret')return;
 try{
  const name=`${r.phase}-${step.id}.png`;
  await page.screenshot({path:path.join(dir,name),timeout,mask:[...masks,...page.frames().map(f=>f.locator('input[type=password]'))],animations:'disabled'});
  r.screenshot='browser/'+name;
  if(failed)r.detail+='。操作が未完了となった後の画面写真。保存の成功を示す記録とは別';
 }catch(error){r.detail+='。画面写真は未取得: '+redact(error.message);}
}
function singleFrame(kind,bytes){
 if(!kind.toLowerCase().startsWith('multipart/related'))throw Error('multipart/relatedではない');
 const boundary=kind.split(';').slice(1).map(x=>x.trim()).find(x=>/^boundary=/i.test(x))?.split('=').slice(1).join('=').replace(/^"|"$/g,'');
 if(!boundary||boundary.length>200||/[\r\n]/.test(boundary))throw Error('boundaryが不正');
 if(bytes.subarray(0,2).equals(Buffer.from('\r\n')))bytes=bytes.subarray(2);
 const opening=Buffer.from(`--${boundary}\r\n`);if(!bytes.subarray(0,opening.length).equals(opening))throw Error('multipart開始が不明');
 bytes=bytes.subarray(opening.length);const split=bytes.indexOf('\r\n\r\n');if(split<0||split>8192||!bytes.subarray(0,split).toString().toLowerCase().split('\r\n').some(x=>x.startsWith('content-type: application/octet-stream')))throw Error('frameのヘッダーが不正');
 const body=bytes.subarray(split+4),end=Buffer.from(`\r\n--${boundary}--`),index=body.indexOf(end);
 if(index<0||!['','\r\n'].includes(body.subarray(index+end.length).toString())||body.subarray(0,index).includes(Buffer.from(`\r\n--${boundary}\r\n`)))throw Error('単一frameの全部を読み出せない');
 return body.subarray(0,index);
}
async function steps(list,phase){
 for(const step of list){active=step.id;const r={id:step.id,phase,completed:false,text:null,screenshot:null,detail:'',attribute:null,visible_slots:[]};report.steps.push(r);
  try{const options={timeout:remaining()};
   if(step.action==='goto'){const url=new URL(replace(step.path),request.origin);if(url.origin!==request.origin)throw Error('操作先のオリジンが不一致');await page.goto(url.href,{...options,waitUntil:'domcontentloaded'});}
   else if(step.action==='fill'){const value=step.value.source==='secret'?secrets[step.value.key]:replaceFor(step,step.value.value);if(typeof value!=='string')throw Error('試験用の認証情報が未設定');const locator=target(step.target);if(step.value.source==='secret')masks.push(locator);await locator.fill(value,options);}
   else if(step.action==='click'){
    if(step.wait_response){
     const watch=spec.responses.find(item=>item.id===step.wait_response);if(!watch)throw Error('待機する応答が未設定');
     const response=page.waitForResponse(r=>{const u=new URL(r.url());return request.origins.includes(u.origin)&&u.pathname===watch.path&&r.request().method()===watch.method;},options);
     await Promise.all([response,target(step.target).click(options)]);
    }else await target(step.target).click(options);
   }
   else if(step.action==='expand'){const content=target(step.content);await target(step.target).waitFor({...options,state:'visible'});if(!await content.isVisible())await target(step.target).click(options);await content.waitFor({...options,state:'visible'});}
   else if(step.action==='dismiss'){if(phase!=='preparation')throw Error('案内を閉じる操作は準備中だけ利用できる');await dismiss(step,r);}
   else if(step.action==='select'){await target(step.target).selectOption(replace(step.value),options);}
   else if(step.action==='read_text'){const base=target(step.target);const locator=step.contains?base.filter({hasText:replace(step.contains)}):base;await locator.waitFor({...options,state:'visible'});const text=await locator.innerText(options);if(text.length>8192)throw Error('表示の文字列が上限を超えた');r.text=redact(text);}
   else if(step.action==='read_attribute'){const locator=target(step.target);await locator.waitFor({...options,state:'attached'});const value=await locator.getAttribute(step.name,options);if(value!==null&&value.length>8192)throw Error('属性の値が上限を超えた');r.attribute={present:value!==null,value:value===null?null:redact(value)};}
   else if(step.action==='read_canvas'){
    const {PNG}=await import(pathToFileURL(process.env.EEL_PNGJS_MODULE).href);
    const locator=target(step.target);await locator.waitFor({...options,state:'visible'});
    if(await locator.evaluate(e=>e.tagName)!=='CANVAS')throw Error('観測先がcanvasではない');
    const until=Date.now()+Math.max(1,Math.min(12000,remaining()-1000));let photo;
    do{
     const attribute='data-eel-canvas-probe',marker=createHash('sha256').update(request.run_id+step.id).digest('hex').slice(0,16),previous=await locator.getAttribute(attribute);
     await locator.evaluate((e,{attribute,marker})=>e.setAttribute(attribute,marker),{attribute,marker});
     try{photo=await locator.screenshot({timeout:remaining(),animations:'disabled',style:`* { visibility: hidden !important; } canvas[${attribute}="${marker}"] { visibility: visible !important; }`});}
     finally{await locator.evaluate((e,{attribute,previous})=>previous==null?e.removeAttribute(attribute):e.setAttribute(attribute,previous),{attribute,previous});}
     if(photo.length>4*1024*1024)throw Error('canvasの画面写真が上限を超えた');
     const png=PNG.sync.read(photo);if(png.width*png.height>2000000)throw Error('canvasの画素数が上限を超えた');
     const levels=new Set();let non_black_pixels=0;
     for(let i=0;i<png.data.length;i+=4){if(!png.data[i+3])continue;const y=Math.round(0.2126*png.data[i]+0.7152*png.data[i+1]+0.0722*png.data[i+2]);levels.add(y);if(y>2)non_black_pixels++;}
     r.canvas={measurement:'isolated_canvas_screenshot',width:png.width,height:png.height,levels:levels.size,non_black_pixels,rgba_sha256:createHash('sha256').update(png.data).digest('hex'),variation:levels.size>=step.min_levels&&non_black_pixels>=step.min_pixels};
     if(r.canvas.variation||Date.now()>=until)break;await new Promise(resolve=>setTimeout(resolve,300));
    }while(true);
    const name=`${phase}-${step.id}.png`;await fs.writeFile(path.join(dir,name),photo);r.screenshot='browser/'+name;
   }
   else throw Error('未対応の操作');
   r.completed=true;r.detail=(r.detail?r.detail+'。':'')+'操作を完了した。保存の成功を示す記録とは別';
   if(step.action!=='read_canvas')await screenshot(step,r,Math.max(1,Math.min(15000,deadline-Date.now())));
   await captureScreen(step,r,phase);
   r.visible_slots=await visibleSlots();
  }catch(error){
   r.detail=(r.detail?r.detail+'。':'')+redact(error.message);report.detail=`${phase}の手順 ${step.id} を完了できなかった`;
   // 期待する文字を待ち切れなかったとき、対象が一意に見えていれば、その時点の表示を参考記録として残す。
   // 手順は未完了のままとし、後続操作を続けたり条件の成功・失敗に変換したりしない。
   if(step.action==='read_text'&&step.contains&&error.name==='TimeoutError'){
    try{
     const base=target(step.target);
     const observed=await Promise.race([(async()=>await base.count()===1&&await base.isVisible()?await base.innerText({timeout:1000}):null)(),new Promise(resolve=>setTimeout(()=>resolve(null),1000))]);
     if(typeof observed==='string'&&observed.length<=8192){r.text=redact(observed);r.detail+='。待機終了後の表示を参考として記録した。手順は未完了';}
    }catch{}
   }
   // 操作の待機上限を使い切っていても、一度だけ短い上限で停止後の表示を残す。
   // 追加の操作や再試行はせず、写真の有無で未完了という結果を変えない。
   await screenshot(step,r,1000,true);return false;
  }
 }
 active=null;return true;
}
try{
 if(process.env.EEL_BROWSER_SECRETS_FILE)secrets=JSON.parse(await fs.readFile(process.env.EEL_BROWSER_SECRETS_FILE,'utf8'));
 const {chromium}=await import(pathToFileURL(process.env.EEL_PLAYWRIGHT_MODULE).href);
 browser=await chromium.launch({channel:'chromium',headless:true,chromiumSandbox:true,timeout:Math.max(1,Math.min(15000,deadline-Date.now())),executablePath:process.env.EEL_BROWSER_EXECUTABLE||undefined,args:report.launch_args});report.version=browser.version();
 if(report.version!==spec.expected_version)throw Error('ブラウザの版が試験設定と異なる');
 const context=await browser.newContext({viewport:{width:1440,height:1000},ignoreHTTPSErrors:spec.tls,serviceWorkers:'block',acceptDownloads:false});
 await context.route('**/*',async route=>{let allowed=false;try{allowed=request.origins.includes(new URL(route.request().url()).origin);}catch{}if(allowed)await route.continue();else{report.blocked_requests++;await route.abort('blockedbyclient');}});
 await context.routeWebSocket('**/*',ws=>ws.close());
 page=await context.newPage();page.on('dialog',d=>d.accept());page.on('download',d=>d.cancel());
 page.on('request',r=>requests.set(r,active));
 page.on('requestfinished',r=>requests.delete(r));page.on('requestfailed',r=>requests.delete(r));
 page.on('response',r=>{
  const p=(async()=>{const request_step=requests.get(r.request());const u=new URL(r.url());if(!request.origins.includes(u.origin))return;
   const observed=await headersOf(r);
   try{if(r.request().isNavigationRequest()&&r.frame()===page.mainFrame())lastDocument={status:r.status(),...observed};}catch{}
   if(!spec.actions.some(s=>s.id===request_step))return;
   // 件数と合計の大きさの両方に上限を置く。操作器の記録全体が基盤の読み取り上限を超えないようにする。
   // 追跡する値が、要求（クエリ文字列と本文）と応答の本文に含まれていたかを調べる。本文は残さない。
   let request_slots=[],response_slots=[],response_unread=false;
   if(tracked.length){
    const sent=r.request().postData()||'';
    request_slots=slotsIn([u.search,decoded(u.search),sent,decoded(sent)]);
    // ページが本文を読まない応答では、本文の取得が終わらないことがある。待つ時間に上限を置き、読めなかったことを記録する。
    const reading=r.body();reading.catch(()=>{});
    try{const body=await Promise.race([reading,new Promise((_,reject)=>setTimeout(()=>reject(Error('timeout')),1500))]);if(body.length<=1024*1024)response_slots=slotsIn([body.toString('utf8')]);else response_unread=true;}catch{response_unread=true;}
   }
   const exchange={step:request_step,method:r.request().method(),path:u.pathname.slice(0,2048),status:r.status(),resource:r.request().resourceType().slice(0,32),...observed,request_slots,response_slots,response_unread},size=JSON.stringify(exchange).length;
   if(report.exchanges.length>=LIMITS.exchanges||exchangeBytes+size>LIMITS.exchangeBytes)report.exchange_limit=true;
   else{exchangeBytes+=size;report.exchanges.push(exchange);}
for(const w of spec.responses){if(w.path!==u.pathname||w.method!==r.request().method())continue;if(report.responses.length>=200){report.response_limit=true;continue;}const record={watch:w.id,method:r.request().method(),path:u.pathname,status:r.status(),body_match:null,request_step,payload_sha256:null,headers:observed.headers};report.responses.push(record);if(w.contains!=null||w.payload){try{const body=await r.body();if(body.length<=1024*1024){if(w.contains!=null)record.body_match=body.toString('utf8').includes(replace(w.contains));if(w.payload){const payload=w.payload==='dicom_frame'?singleFrame(r.headers()['content-type']||'',body):body;record.payload_sha256=createHash('sha256').update(payload).digest('hex');}}}catch{}}}
  })().catch(()=>{});pending.add(p);p.finally(()=>pending.delete(p));
 });
 await page.goto(new URL(spec.start_path,request.origin).href,{timeout:remaining(),waitUntil:'domcontentloaded'});
 report.prepared=await steps(spec.preparation,'preparation');await save('ready.json',report);
 if(report.prepared){while(true){if(await fs.stat(path.join(dir,'stop')).then(()=>true,()=>false))throw Error('停止を要求された');if(await fs.stat(path.join(dir,'continue.json')).then(()=>true,()=>false)){const control=JSON.parse(await fs.readFile(path.join(dir,'continue.json'),'utf8'));if(control.run_id!==report.run_id||control.proceed!==true)throw Error('継続する試行が一致しない');break;}remaining();await new Promise(r=>setTimeout(r,30));}
  // 準備中の通信と、故障後の操作で観測した通信を混ぜない。
  await Promise.all([...pending]);report.responses=[];report.exchanges=[];report.exchange_limit=false;exchangeBytes=0;requests.clear();report.completed=await steps(spec.actions,'action');
 }
 await Promise.race([Promise.all([...pending]),new Promise(r=>setTimeout(r,Math.min(2000,Math.max(1,deadline-Date.now()))))]);
}catch(error){report.detail=redact(error.message);}finally{clearInterval(stopPoll);await closeBrowser();if(!report.detail)report.detail=report.completed?'宣言した操作を完了した。画面・HTTP・DBの記録を別々に確認する':'操作を完了していない';
 if(!await fs.stat(path.join(dir,'ready.json')).then(()=>true,()=>false))await save('ready.json',report);
 await save('screens-raw.json',{schema_version:1,run_id:report.run_id,screens,truncated:screensTruncated}).catch(()=>{});
 await save('result.json',report);
}
