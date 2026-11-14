#!/usr/bin/env node
// Runs the shipped inline UI against a DOM and fake API; no real node mutations.
// Pass a separately installed jsdom module directory as argv[2].
'use strict';
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const {JSDOM,VirtualConsole}=require(process.argv[2]||'jsdom');
const html=fs.readFileSync(path.join(__dirname,'../service-navigation.html'),'utf8');
const navigation=JSON.parse(html.match(/<script id="navigation-data" type="application\/json">([\s\S]*?)<\/script>/)[1]);
const intervals=new Map(),errors=[],passed=[];
let nextTimer=1,focused=true,hidden=false,stateReads=0,lastPreview=null,catalogReads=0,holdCatalog=false,releaseCatalog=null,previewError=false,allowApplyFixture=false,apiCalls=0;
const peers=[...new Set(navigation.services.filter(s=>s.remote).map(s=>s.peerNodeId))];
const normalNodes=peers.map((id,n)=>({id,label:n?'Windows':'Ubuntu',phase:'connected',rttMs:10,mappings:1}));
let activeNavigation=navigation,activeNodes=normalNodes;
const consoleSink=new VirtualConsole();
consoleSink.on('jsdomError',e=>{if(e.type==='unhandled-exception'||/Uncaught/.test(e.message))errors.push(e)});
const dom=new JSDOM(html,{url:'http://127.0.0.1:3001/',runScripts:'dangerously',pretendToBeVisual:true,virtualConsole:consoleSink,beforeParse(window){
 Object.defineProperty(window.document,'hidden',{configurable:true,get:()=>hidden});
 Object.defineProperty(window.document,'hasFocus',{configurable:true,value:()=>focused});
 window.matchMedia=query=>({matches:false,media:query,addEventListener(){},removeEventListener(){}});
 window.HTMLElement.prototype.scrollIntoView=function(){};
 window.confirm=()=>{throw Error('Native confirmation dialogs must not be used')};
 window.setInterval=(fn,ms)=>{const id=nextTimer++;intervals.set(id,{fn,ms});return id};
 window.clearInterval=id=>intervals.delete(id);
 window.fetch=async(url,options={})=>{
  apiCalls++;
  if(url==='/api/navigation/bootstrap')return {ok:true,status:200,json:async()=>({token:'test-session'})};
  if(url==='/api/navigation/catalog'){
   catalogReads++;if(holdCatalog)await new Promise(resolve=>{releaseCatalog=resolve});
   return {ok:true,status:200,json:async()=>({result:{editableTarget:JSON.parse(options.body).peer===peers[0],services:[{id:'preview-service',name:'Preview service',target:'127.0.0.1:8080'}]}})};
  }
  if(url==='/api/navigation/preview'){
   lastPreview=JSON.parse(options.body);
   return previewError?{ok:false,status:400,json:async()=>({error:{code:'validation',message:'测试：本机端口已被占用'}})}:{ok:true,status:200,json:async()=>({result:{plan:'preview-only',changes:['Preview only'],warnings:[]}})};
  }
  if(url==='/api/navigation/apply'&&allowApplyFixture)return {ok:true,status:200,json:async()=>({result:{message:'Fixture save complete',warnings:[]}})};
  assert.equal(url,'/api/navigation/state','DOM checks must never issue a mutation');
  stateReads++;
  return {ok:true,status:200,json:async()=>({result:{navigation:activeNavigation,revision:28,checkedAt:new Date(Date.UTC(2026,10,7,9,0,stateReads)).toISOString(),agent:{available:true,running:true,activeStreams:3,diagnostics:Array(32).fill('信令心跳已确认'),signal:'online'},rules:activeNavigation.services.filter(s=>s.remote).map(s=>({id:s.id,listening:true,peer_ready:true})),nodes:activeNodes,remoteConfigAvailable:false,errors:[],history:[]}})};
 };
}});
const {window}=dom,document=window.document;
const ui=id=>document.getElementById(id);
async function flush(){for(let i=0;i<4;i++)await new Promise(resolve=>setImmediate(resolve))}
async function check(name,fn){await fn();assert.equal(errors.length,0,errors.map(e=>e.message).join('\n'));passed.push(name);console.log('PASS '+name)}
function enable(){focused=true;hidden=false;ui('manage-monitor').checked=true;ui('manage-monitor').dispatchEvent(new window.Event('change',{bubbles:true}));assert.equal(ui('manage-monitor').checked,true);assert.equal(intervals.size,1);return [...intervals.values()][0].fn}
function assertStopped(){assert.equal(ui('manage-monitor').checked,false);assert.equal(intervals.size,0)}
function filterButton(value){return [...ui('filters').querySelectorAll('.tab')].find(button=>button.dataset.filter===value)}
const ubuntuPeer=navigation.services.find(s=>s.serviceId==='ubuntu-admin').peerNodeId;
const windowsPeer=navigation.services.find(s=>s.remoteLabel==='远端 Windows').peerNodeId;
function isNodeService(s){return s.serviceId==='ubuntu-admin'||s.serviceId==='mac-admin'}
function peerTabs(){return [...ui('peer-filters').querySelectorAll('[role="tab"]')]}
function peerButton(peer){return peerTabs().find(button=>button.dataset.peer===peer)}
function visibleServices(){return [...ui('services').querySelectorAll('.card')].map(card=>navigation.services.find(s=>s.id===card.dataset.ruleId))}
function assertVisiblePeer(peer){const expected=navigation.services.filter(s=>!isNodeService(s)&&s.peerNodeId===peer);assert.deepEqual(visibleServices().map(s=>s.id).sort(),expected.map(s=>s.id).sort())}
(async()=>{
 try{
  await flush();
  await check('connects with monitoring off by default',()=>{assert.equal(ui('manage-add').disabled,false);assertStopped();assert.equal(stateReads,1)});
  await check('machine tabs isolate every service category while node management stays above them',()=>{
   assert.equal(peerTabs().length,2);
   assert.match(peerButton(ubuntuPeer).textContent,/Ubuntu.*Mac/);
   assert.match(peerButton(windowsPeer).textContent,/Windows.*Mac/);
   assert.equal(peerButton(ubuntuPeer).getAttribute('aria-selected'),'true');
   assertVisiblePeer(ubuntuPeer);
   assert.equal(ui('node-services').querySelectorAll('.card').length,2);
   assert.ok(ui('node-services').compareDocumentPosition(ui('peer-filters'))&window.Node.DOCUMENT_POSITION_FOLLOWING);
   peerButton(windowsPeer).click();assertVisiblePeer(windowsPeer);
   assert.equal(ui('node-services').querySelectorAll('.card').length,2);
   assert.equal(filterButton('节点管理'),undefined);
   peerButton(ubuntuPeer).click();assertVisiblePeer(ubuntuPeer);
  });
  await check('machine switching resets category and search and never mixes peers',()=>{
   filterButton('应用').click();
   assert.ok(visibleServices().every(s=>s.peerNodeId===ubuntuPeer&&s.category==='应用'));
   assert.equal(ui('services').querySelectorAll('.service-group').length,1);
   ui('search').value='streamdesk-wsl';ui('search').dispatchEvent(new window.Event('input',{bubbles:true}));
   assert.equal(visibleServices().length,0);
   assert.equal(ui('node-services').querySelectorAll('.card').length,2);
   peerButton(windowsPeer).click();
   assert.equal(ui('search').value,'');
   assert.equal(filterButton('全部').getAttribute('aria-pressed'),'true');
   assertVisiblePeer(windowsPeer);
   ui('search').value='streamdesk-wsl';ui('search').dispatchEvent(new window.Event('input',{bubbles:true}));
   assert.equal(visibleServices().length,3);
   assert.ok(visibleServices().every(s=>s.peerNodeId===windowsPeer));
   peerButton(ubuntuPeer).click();assertVisiblePeer(ubuntuPeer);
  });
  await check('machine tabs support keyboard navigation and retain selection across status refresh',async()=>{
   peerButton(ubuntuPeer).dispatchEvent(new window.KeyboardEvent('keydown',{key:'ArrowRight',bubbles:true}));
   assert.equal(peerButton(windowsPeer).getAttribute('aria-selected'),'true');
   assert.equal(document.activeElement,peerButton(windowsPeer));
   assertVisiblePeer(windowsPeer);
   ui('manage-refresh').click();await flush();
   assert.equal(peerButton(windowsPeer).getAttribute('aria-selected'),'true');
   assertVisiblePeer(windowsPeer);
   peerButton(windowsPeer).dispatchEvent(new window.KeyboardEvent('keydown',{key:'Home',bubbles:true}));
   assert.equal(peerButton(ubuntuPeer).getAttribute('aria-selected'),'true');
   assert.equal(document.activeElement,peerButton(ubuntuPeer));
   assertVisiblePeer(ubuntuPeer);
  });
  await check('activating a device tab preserves focus on that device',()=>{
   peerButton(windowsPeer).focus();peerButton(windowsPeer).click();
   assert.equal(document.activeElement,peerButton(windowsPeer));
   peerButton(ubuntuPeer).focus();peerButton(ubuntuPeer).click();
   assert.equal(document.activeElement,peerButton(ubuntuPeer));
  });
  await check('activating a category preserves focus on that category',()=>{
   filterButton('应用').focus();filterButton('应用').click();
   assert.equal(document.activeElement,filterButton('应用'));
   filterButton('全部').focus();filterButton('全部').click();
   assert.equal(document.activeElement,filterButton('全部'));
  });
  await check('foreground status updates retain a focused category without stealing search focus',async()=>{
   const tick=enable();filterButton('全部').focus();tick();await flush();
   assert.equal(document.activeElement,filterButton('全部'));
   ui('search').focus();tick();await flush();assert.equal(document.activeElement,ui('search'));
   ui('manage-monitor').checked=false;ui('manage-monitor').dispatchEvent(new window.Event('change',{bubbles:true}));assertStopped();
  });
  await check('find services moves keyboard focus directly to search',()=>{
   ui('find-services').focus();ui('find-services').click();
   assert.equal(document.activeElement,ui('search'));
  });
  await check('search scope and result status follow the selected remote device',()=>{
   peerButton(windowsPeer).click();
   assert.match(ui('search').placeholder,/Windows/);
   assert.match(ui('search').getAttribute('aria-label'),/Windows/);
   const status=ui('service-results-status');
   assert.equal(status.getAttribute('role'),'status');assert.equal(status.getAttribute('aria-live'),'polite');
   assert.match(status.textContent,/Windows/);
   ui('search').value='streamdesk-wsl';ui('search').dispatchEvent(new window.Event('input',{bubbles:true}));
   assert.equal(visibleServices().length,3);assert.match(status.textContent,/3\s*\/\s*6/);
   peerButton(ubuntuPeer).click();
   assert.match(ui('search').placeholder,/Ubuntu/);assert.match(status.textContent,/Ubuntu/);
  });
  await check('empty search can explicitly move the same query to another machine',()=>{
   filterButton('应用').click();ui('search').value='streamdesk-wsl';ui('search').dispatchEvent(new window.Event('input',{bubbles:true}));
   assert.equal(visibleServices().length,0);assert.match(ui('services').textContent,/Ubuntu/);
   const other=ui('services').querySelector('[data-search-peer="'+windowsPeer+'"]');assert.ok(other);other.click();
   assert.equal(ui('search').value,'streamdesk-wsl');assert.equal(filterButton('全部').getAttribute('aria-pressed'),'true');
   assert.equal(peerButton(windowsPeer).getAttribute('aria-selected'),'true');
   assert.equal(visibleServices().length,3);assert.ok(visibleServices().every(s=>s.peerNodeId===windowsPeer));
   assert.equal(document.activeElement,ui('search'));
   peerButton(ubuntuPeer).click();assert.equal(ui('search').value,'');
  });
  await check('empty search renders user text safely and clears both search and category',()=>{
   filterButton('应用').click();const query='<img src=x onerror="alert(1)">';ui('search').value=query;
   ui('search').dispatchEvent(new window.Event('input',{bubbles:true}));
   assert.equal(ui('services').querySelector('img'),null);assert.ok(ui('services').textContent.includes(query));
   ui('clear-service-filters').click();
   assert.equal(ui('search').value,'');assert.equal(filterButton('全部').getAttribute('aria-pressed'),'true');
   assertVisiblePeer(ubuntuPeer);assert.equal(document.activeElement,ui('search'));
  });
  await check('unchanged status refresh does not repeat live search-result announcements',async()=>{
   const changes=[],observer=new window.MutationObserver(records=>changes.push(...records));
   observer.observe(ui('service-results-status'),{childList:true,characterData:true,subtree:true});
   try{ui('manage-refresh').click();await flush();assert.equal(changes.length,0)}
   finally{observer.disconnect()}
  });
  await check('adding a mapping starts with the machine selected in the tabs',async()=>{
   peerButton(windowsPeer).click();ui('manage-add').click();await flush();
   assert.equal(ui('mapping-peer').value,windowsPeer);
   assert.equal(ui('mapping-peer').disabled,true);
   assert.match(ui('editor-title').textContent,/Windows.*Mac/);
   assert.ok(ui('mapping-editor').contains(document.activeElement));assert.equal(document.activeElement.disabled,false);
   ui('editor-close').click();peerButton(ubuntuPeer).click();
  });
  await check('global status actions and direction-specific connection work have distinct scopes',()=>{
   const workspace=ui('connection-workspace');assert.ok(workspace);
   for(const id of ['peer-filters','filters','search','manage-add','mapping-editor','services','connection-message'])assert.ok(workspace.contains(ui(id)),id);
   assert.ok(ui('navigation-management').contains(ui('manage-refresh')));
   assert.equal(ui('navigation-management').contains(ui('mapping-editor')),false);
   assert.equal(workspace.contains(ui('node-services')),false);
   assert.match(ui('manage-add').textContent,/新建端口连接/);
   peerButton(windowsPeer).click();assert.match(ui('manage-add').getAttribute('aria-label'),/Windows.*Mac/);
   peerButton(ubuntuPeer).click();assert.match(ui('manage-add').getAttribute('aria-label'),/Ubuntu.*Mac/);
  });
  await check('an open draft cannot silently change devices or be replaced by another editor',async()=>{
   ui('manage-add').click();await flush();ui('mapping-name').value='Keep this Ubuntu draft';
   const reads=catalogReads;
   peerButton(windowsPeer).click();
   assert.equal(peerButton(ubuntuPeer).getAttribute('aria-selected'),'true');
   assert.equal(ui('mapping-peer').value,ubuntuPeer);assert.equal(ui('mapping-name').value,'Keep this Ubuntu draft');
   peerButton(ubuntuPeer).focus();
   peerButton(ubuntuPeer).dispatchEvent(new window.KeyboardEvent('keydown',{key:'ArrowRight',bubbles:true}));
   assert.equal(document.activeElement,peerButton(ubuntuPeer));
   assert.equal(peerButton(ubuntuPeer).getAttribute('aria-selected'),'true');
   ui('manage-add').click();
   const another=ui('services').querySelector('.rule-controls>button');assert.ok(another);another.click();
   await flush();assert.equal(catalogReads,reads);assert.equal(ui('mapping-name').value,'Keep this Ubuntu draft');
   ui('editor-close').click();
   assert.equal(ui('mapping-editor').hidden,true);assert.equal(ui('manage-add').disabled,false);
   peerButton(windowsPeer).click();assert.equal(peerButton(windowsPeer).getAttribute('aria-selected'),'true');
   peerButton(ubuntuPeer).click();
  });
  await check('direction changes through empty-search shortcuts cannot discard an open draft',async()=>{
   ui('manage-add').click();await flush();ui('mapping-name').value='Keep draft during search';
   ui('search').value='unmatched-fixture-search';ui('search').dispatchEvent(new window.Event('input',{bubbles:true}));
   const other=ui('services').querySelector('[data-search-peer="'+windowsPeer+'"]');assert.ok(other);other.click();
   assert.equal(peerButton(ubuntuPeer).getAttribute('aria-selected'),'true');
   assert.equal(ui('mapping-name').value,'Keep draft during search');
   assert.equal(ui('search').value,'unmatched-fixture-search');
   ui('editor-close').click();ui('clear-service-filters').click();
  });
  await check('moved editor remains disabled during catalog loading and locks the chosen device afterwards',async()=>{
   holdCatalog=true;ui('manage-add').click();await flush();
   try{
    assert.equal(ui('mapping-editor').hidden,false);
    for(const id of ['mapping-name','mapping-port','mapping-peer','mapping-preview'])assert.equal(ui(id).disabled,true,id);
   }finally{holdCatalog=false;releaseCatalog?.();await flush()}
   assert.equal(ui('mapping-name').disabled,false);assert.equal(ui('mapping-peer').disabled,true);
   ui('editor-close').click();assert.equal(document.activeElement,ui('manage-add'));
  });
  await check('preview feedback and errors stay beside the active connection draft',async()=>{
   ui('manage-add').click();await flush();
   ui('mapping-form').dispatchEvent(new window.Event('submit',{cancelable:true,bubbles:true}));await flush();
   assert.equal(ui('mapping-preview-box').hidden,false);assert.match(ui('connection-message').textContent,/检查|确认|预览/);
   const globalMessage=ui('manage-message').textContent;
   previewError=true;
   try{
    ui('mapping-form').dispatchEvent(new window.Event('submit',{cancelable:true,bubbles:true}));await flush();
    assert.match(ui('connection-message').textContent,/端口已被占用/);
    assert.equal(ui('connection-message').classList.contains('error'),true);
    assert.equal(ui('manage-message').textContent,globalMessage);
    assert.equal(ui('mapping-editor').hidden,false);
   }finally{previewError=false;ui('editor-close').click()}
  });
  await check('editing the fixed Ubuntu management mapping selects its direction first',async()=>{
   peerButton(windowsPeer).click();
   const node=ui('node-services').querySelector('[data-service-id="ubuntu-admin"]');
   node.querySelector('.rule-controls>button').click();await flush();
   assert.equal(peerButton(ubuntuPeer).getAttribute('aria-selected'),'true');
   assert.equal(ui('mapping-peer').value,ubuntuPeer);assert.equal(ui('mapping-peer').disabled,true);
   assert.match(ui('editor-title').textContent,/Ubuntu.*Mac/);assert.equal(ui('mapping-target').disabled,true);
   ui('editor-close').click();
  });
  await check('a service confirmation stays in its connection workspace and cancel sends no mutation',()=>{
   const more=ui('services').querySelector('.rule-controls .popover');more.open=true;
   more.querySelector('.menu-panel button').click();
   assert.equal(ui('manage-confirmation').hidden,false);
   assert.ok(ui('connection-workspace').contains(ui('manage-confirmation')));
   ui('manage-confirm-cancel').click();assert.equal(ui('manage-confirmation').hidden,true);
  });
  await check('a pending service confirmation keeps its device direction until cancelled',()=>{
   const more=ui('services').querySelector('.rule-controls .popover');more.open=true;more.querySelector('.menu-panel button').click();
   peerButton(windowsPeer).click();assert.equal(peerButton(ubuntuPeer).getAttribute('aria-selected'),'true');
   peerButton(ubuntuPeer).focus();peerButton(ubuntuPeer).dispatchEvent(new window.KeyboardEvent('keydown',{key:'ArrowRight',bubbles:true}));
   assert.equal(document.activeElement,peerButton(ubuntuPeer));assert.equal(peerButton(ubuntuPeer).getAttribute('aria-selected'),'true');
   ui('search').value='unmatched-fixture-search';ui('search').dispatchEvent(new window.Event('input',{bubbles:true}));
   ui('services').querySelector('[data-search-peer="'+windowsPeer+'"]').click();
   assert.equal(peerButton(ubuntuPeer).getAttribute('aria-selected'),'true');assert.match(ui('connection-message').textContent,/确认|取消/);
   ui('manage-confirm-cancel').click();peerButton(windowsPeer).click();
   assert.equal(peerButton(windowsPeer).getAttribute('aria-selected'),'true');peerButton(ubuntuPeer).click();
  });
  await check('refresh cancels a service confirmation when its device and mapping disappear',async()=>{
   peerButton(windowsPeer).click();
   const more=ui('services').querySelector('.rule-controls .popover');more.open=true;more.querySelector('.menu-panel button').click();
   assert.equal(ui('manage-confirmation').hidden,false);
   activeNodes=normalNodes.filter(n=>n.id!==windowsPeer);activeNavigation={...navigation,services:navigation.services.filter(s=>s.peerNodeId!==windowsPeer)};
   try{
    ui('manage-refresh').click();await flush();assert.equal(ui('manage-confirmation').hidden,true);
    assert.match(ui('connection-message').textContent,/确认.*取消|取消.*确认/);
    const calls=apiCalls;ui('manage-confirm-run').click();await flush();assert.equal(apiCalls,calls,'cancelled confirmation cannot send an action');
    assert.equal(peerButton(ubuntuPeer).getAttribute('aria-selected'),'true');
   }finally{activeNodes=normalNodes;activeNavigation=navigation;ui('manage-refresh').click();await flush();peerButton(ubuntuPeer).click()}
  });
  await check('a fixed-node card confirmation opens under its actual device direction',()=>{
   peerButton(windowsPeer).click();
   const more=ui('node-services').querySelector('[data-service-id="ubuntu-admin"] .rule-controls .popover');more.open=true;more.querySelector('.menu-panel button').click();
   assert.equal(ui('manage-confirmation').hidden,false);
   assert.equal(peerButton(ubuntuPeer).getAttribute('aria-selected'),'true');
   assert.ok(ui('connection-workspace').contains(ui('manage-confirmation')));
   ui('manage-confirm-cancel').click();
  });
  await check('known devices without ordinary mappings still have a direction and can create a connection',async()=>{
   const emptyPeer='eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';
   activeNodes=[...normalNodes,{id:emptyPeer,label:'Test Linux',phase:'connected',rttMs:5,mappings:0}];
   activeNavigation={...navigation,services:navigation.services.filter(s=>s.peerNodeId!==ubuntuPeer||isNodeService(s))};
   try{
    ui('manage-refresh').click();await flush();assert.ok(peerButton(ubuntuPeer));peerButton(ubuntuPeer).click();
    assert.equal(visibleServices().length,0);assert.equal(ui('manage-add').disabled,false);
    ui('manage-add').click();await flush();assert.equal(ui('mapping-peer').value,ubuntuPeer);assert.equal(ui('mapping-mode-new').disabled,false);
    ui('editor-close').click();
    assert.ok(peerButton(emptyPeer));peerButton(emptyPeer).click();assert.equal(visibleServices().length,0);
    assert.equal(ui('manage-add').disabled,false);ui('manage-add').click();await flush();
    assert.equal(ui('mapping-peer').value,emptyPeer);assert.equal(ui('mapping-peer').disabled,true);
    ui('editor-close').click();
   }finally{if(!ui('mapping-editor').hidden)ui('editor-close').click();activeNavigation=navigation;activeNodes=normalNodes;ui('manage-refresh').click();await flush();peerButton(ubuntuPeer).click()}
  });
  await check('a disappearing device cannot move an open draft or leave an old preview actionable',async()=>{
   peerButton(windowsPeer).click();ui('manage-add').click();await flush();ui('mapping-name').value='Retain Windows draft';
   ui('mapping-form').dispatchEvent(new window.Event('submit',{cancelable:true,bubbles:true}));await flush();
   assert.equal(ui('mapping-preview-box').hidden,false);
   activeNodes=normalNodes.filter(n=>n.id!==windowsPeer);activeNavigation={...navigation,services:navigation.services.filter(s=>s.peerNodeId!==windowsPeer)};
   try{
    ui('manage-refresh').click();await flush();
    assert.ok(peerButton(windowsPeer));assert.equal(peerButton(windowsPeer).getAttribute('aria-selected'),'true');
    assert.equal(ui('mapping-peer').value,windowsPeer);assert.equal(ui('mapping-name').value,'Retain Windows draft');
    assert.equal(ui('mapping-preview').disabled,true);assert.equal(ui('mapping-apply').disabled,true);
    assert.equal(ui('mapping-preview-box').hidden,true);
    assert.match(ui('connection-workspace').textContent,/不可管理|无法确认|不再|消失|移除/);
   }finally{
    activeNavigation=navigation;activeNodes=normalNodes;ui('manage-refresh').click();await flush();
    assert.equal(ui('mapping-name').value,'Retain Windows draft');assert.equal(ui('mapping-preview').disabled,false);
    ui('editor-close').click();peerButton(ubuntuPeer).click();
   }
  });
  await check('closing an editor restores focus to its rebuilt service-card action',async()=>{
   const card=ui('services').querySelector('.card'),rule=card.dataset.ruleId;card.querySelector('.rule-controls>button').click();await flush();
   ui('manage-refresh').click();await flush();ui('editor-close').click();
   const rebuilt=[...ui('services').querySelectorAll('.card')].find(c=>c.dataset.ruleId===rule);
   assert.equal(document.activeElement,rebuilt.querySelector('.rule-controls>button'));
  });
  await check('a simulated successful save restores focus after busy and card reconstruction',async()=>{
   const card=ui('services').querySelector('.card'),rule=card.dataset.ruleId;card.querySelector('.rule-controls>button').click();await flush();
   ui('mapping-form').dispatchEvent(new window.Event('submit',{cancelable:true,bubbles:true}));await flush();
   allowApplyFixture=true;
   try{
    ui('mapping-apply').click();await flush();assert.equal(ui('mapping-editor').hidden,true);
    const rebuilt=[...ui('services').querySelectorAll('.card')].find(c=>c.dataset.ruleId===rule);
    assert.equal(document.activeElement,rebuilt.querySelector('.rule-controls>button'));assert.equal(document.activeElement.disabled,false);
    assert.match(ui('connection-message').textContent,/Fixture save complete/);
   }finally{allowApplyFixture=false;if(!ui('mapping-editor').hidden)ui('editor-close').click()}
  });
  await check('status refresh retains private login controls within the selected machine',async()=>{
   const chromium=navigation.services.find(s=>s.serviceId==='chromium-web'),priorLogin=chromium.login;
   chromium.login={username:'fixture-user',password:'fixture-password'};
   ui('manage-refresh').click();await flush();
   let card=document.querySelector('[data-service-id="chromium-web"]');
   card.querySelector('.service-extra').open=true;
   ui('manage-refresh').click();await flush();
   card=document.querySelector('[data-service-id="chromium-web"]');
   assertVisiblePeer(ubuntuPeer);
   assert.equal(card.querySelector('.service-extra').open,true);
   assert.equal(card.querySelectorAll('.login-help .login-row').length,2);
   assert.equal(card.querySelectorAll('.login-help button').length,2);
   if(priorLogin===undefined)delete chromium.login;else chromium.login=priorLogin;
   ui('manage-refresh').click();await flush();
  });
  await check('theme changes preserve the selected machine, search, editor draft and monitoring',async()=>{
   peerButton(windowsPeer).click();filterButton('应用').click();
   ui('search').value='streamdesk-wsl';ui('search').dispatchEvent(new window.Event('input',{bubbles:true}));
   ui('manage-add').click();await flush();ui('mapping-name').value='Theme switching draft';enable();
   const before=stateReads;
   ui('theme-toggle').click();
   assert.equal(document.documentElement.dataset.theme,'dark');
   assert.equal(window.localStorage.getItem('veytrane-navigation-theme'),'dark');
   assert.equal(peerButton(windowsPeer).getAttribute('aria-selected'),'true');
   assert.equal(filterButton('应用').getAttribute('aria-pressed'),'true');
   assert.equal(ui('search').value,'streamdesk-wsl');assert.equal(visibleServices().length,3);
   assert.equal(ui('mapping-name').value,'Theme switching draft');
   assert.equal(ui('mapping-editor').hidden,false);
   assert.equal(ui('manage-monitor').checked,true);assert.equal(intervals.size,1);assert.equal(stateReads,before);
   ui('manage-refresh').click();await flush();
   assert.equal(document.documentElement.dataset.theme,'dark');
   ui('theme-toggle').click();assert.equal(document.documentElement.dataset.theme,'light');
   ui('manage-monitor').checked=false;ui('manage-monitor').dispatchEvent(new window.Event('change',{bubbles:true}));assertStopped();
   ui('editor-close').click();peerButton(ubuntuPeer).click();
  });
  await check('activity stays collapsed until requested',()=>{assert.equal(ui('manage-activity').hidden,true);assert.equal(ui('manage-activity-toggle').getAttribute('aria-expanded'),'false');ui('manage-activity-toggle').click();assert.equal(ui('manage-activity').hidden,false);ui('manage-activity-toggle').click();assert.equal(ui('manage-activity').hidden,true)});
  await check('diagnostic repeats are consolidated',()=>{assert.equal(ui('manage-diagnostics').textContent,'信令心跳已确认（32 次）')});
  await check('activity tabs expose only the selected view',()=>{ui('activity-diagnostics-tab').click();assert.equal(ui('activity-history-panel').hidden,true);assert.equal(ui('activity-diagnostics-panel').hidden,false);assert.equal(ui('activity-diagnostics-tab').getAttribute('aria-selected'),'true');ui('activity-history-tab').click()});
  await check('reconnect controls stay beside their own node',()=>{assert.equal(document.querySelectorAll('.node-signal [data-reconnect]').length,peers.length);for(const b of document.querySelectorAll('[data-reconnect]'))assert.ok(b.closest('.node-signal').querySelector('.node-info strong'))});
  await check('restart confirmation is secondary and cancel sends no request',()=>{ui('management-tools').open=true;ui('manage-restart').click();assert.equal(ui('management-tools').open,false);assert.equal(ui('manage-confirmation').hidden,false);assert.ok(ui('navigation-management').contains(ui('manage-confirmation')));assert.match(ui('manage-confirm-text').textContent,/全部/);ui('manage-confirm-cancel').click();assert.equal(ui('manage-confirmation').hidden,true)});
  await check('configuration draft survives a status refresh',async()=>{ui('manage-add').click();await flush();assert.equal(ui('mapping-editor').hidden,false);ui('mapping-name').value='Unsaved draft';ui('manage-refresh').click();await flush();assert.equal(ui('mapping-name').value,'Unsaved draft');assert.equal(ui('mapping-editor').hidden,false);ui('editor-close').click();assert.equal(ui('mapping-editor').hidden,true)});
  await check('existing service target is read-only until explicitly edited',async()=>{ui('manage-add').click();await flush();assert.equal(ui('mapping-target').readOnly,true);assert.equal(ui('mapping-name').value,'Preview service');ui('mapping-edit-target').click();assert.equal(ui('mapping-target').readOnly,false);ui('editor-close').click()});
  await check('new service has its own mode and a generated editable ID',async()=>{ui('manage-add').click();await flush();ui('mapping-mode-new').click();assert.equal(ui('mapping-mode-new').getAttribute('aria-pressed'),'true');assert.equal(ui('mapping-service-wrap').hidden,true);assert.equal(ui('mapping-new-wrap').hidden,false);assert.match(ui('mapping-new-id').value,/^service-[a-f0-9]{8}$/);assert.equal(ui('mapping-target').readOnly,false);assert.equal(ui('mapping-name').value,'');ui('editor-close').click()});
  await check('switching service modes preserves a new target draft',async()=>{ui('manage-add').click();await flush();ui('mapping-mode-new').click();const id=ui('mapping-new-id').value;ui('mapping-target').value='127.0.0.1:9876';ui('mapping-target').dispatchEvent(new window.Event('input',{bubbles:true}));ui('mapping-mode-existing').click();assert.equal(ui('mapping-target').value,'127.0.0.1:8080');ui('mapping-mode-new').click();assert.equal(ui('mapping-target').value,'127.0.0.1:9876');assert.equal(ui('mapping-new-id').value,id);ui('editor-close').click()});
  await check('local port and access method update the actual draft and route',async()=>{ui('manage-add').click();await flush();ui('mapping-port').value='23456';ui('mapping-port').dispatchEvent(new window.Event('input',{bubbles:true}));assert.equal(ui('mapping-listen').value,'127.0.0.1:23456');assert.equal(ui('mapping-route-destination').textContent,'127.0.0.1:23456');ui('mapping-protocol').value='http';ui('mapping-protocol').dispatchEvent(new window.Event('change',{bubbles:true}));ui('mapping-path').value='/console';ui('mapping-path').dispatchEvent(new window.Event('input',{bubbles:true}));assert.equal(ui('mapping-access-address').textContent,'http://127.0.0.1:23456/console');assert.equal(ui('mapping-path-wrap').hidden,false);ui('mapping-protocol').value='tcp';ui('mapping-protocol').dispatchEvent(new window.Event('change',{bubbles:true}));assert.equal(ui('mapping-access-address').textContent,'127.0.0.1:23456');assert.equal(ui('mapping-path-wrap').hidden,true);ui('editor-close').click()});
  await check('advanced loopback IPv6 remains intact when editing a port',async()=>{ui('manage-add').click();await flush();ui('mapping-bind-host').value='::1';ui('mapping-bind-host').dispatchEvent(new window.Event('input',{bubbles:true}));ui('mapping-port').value='23457';ui('mapping-port').dispatchEvent(new window.Event('input',{bubbles:true}));assert.equal(ui('mapping-listen').value,'[::1]:23457');ui('editor-close').click()});
  await check('devices without remote configuration access cannot create services',async()=>{peerButton(windowsPeer).click();ui('manage-add').click();await flush();assert.equal(ui('mapping-peer').value,windowsPeer);assert.equal(ui('mapping-mode-new').disabled,true);assert.equal(ui('mapping-target-wrap').hidden,true);ui('mapping-mode-new').click();assert.equal(ui('mapping-service').value,'preview-service');ui('editor-close').click();peerButton(ubuntuPeer).click()});
  await check('new-service preview sends the unchanged Agent contract with the composed port',async()=>{ui('manage-add').click();await flush();ui('mapping-mode-new').click();ui('mapping-name').value='New preview service';ui('mapping-target').value='127.0.0.1:8088';ui('mapping-target').dispatchEvent(new window.Event('input',{bubbles:true}));ui('mapping-port').value='23600';ui('mapping-port').dispatchEvent(new window.Event('input',{bubbles:true}));ui('mapping-form').dispatchEvent(new window.Event('submit',{cancelable:true,bubbles:true}));await flush();assert.equal(lastPreview.listen,'127.0.0.1:23600');assert.equal(lastPreview.target,'127.0.0.1:8088');assert.equal(lastPreview.name,'New preview service');assert.match(lastPreview.serviceId,/^service-[a-f0-9]{8}$/);assert.equal(lastPreview.protocol,'tcp');assert.equal(lastPreview.enabled,true);assert.equal(ui('mapping-preview-box').hidden,false);ui('editor-close').click()});
  await check('search preserves runtime state and all mapping controls',()=>{ui('search').value='nginx-share';ui('search').dispatchEvent(new window.Event('input',{bubbles:true}));assert.equal(ui('services').querySelectorAll('.rule-controls').length,1);assert.equal(ui('services').querySelectorAll('.rule-controls button').length,4);assert.ok(ui('services').querySelector('.runtime-status'))});
  await check('clearing search restores controls on every mapping for the selected machine',()=>{ui('search').value='';ui('search').dispatchEvent(new window.Event('input',{bubbles:true}));assert.equal(ui('services').querySelectorAll('.rule-controls').length,navigation.services.filter(s=>s.remote&&!isNodeService(s)&&s.peerNodeId===ubuntuPeer).length);assert.equal(ui('node-services').querySelectorAll('.rule-controls').length,1)});
  await check('status refresh preserves an expanded service detail',async()=>{const card=ui('services').querySelector('.card');const rule=card.dataset.ruleId;card.querySelector('.service-extra').open=true;ui('manage-refresh').click();await flush();assert.equal([...ui('services').querySelectorAll('.card')].find(c=>c.dataset.ruleId===rule).querySelector('.service-extra').open,true)});
  await check('foreground monitoring reads status and remains enabled',async()=>{const tick=enable(),before=stateReads;tick();await flush();assert.equal(stateReads,before+1);assert.equal(ui('manage-monitor').checked,true)});
  await check('manual off clears the interval',()=>{ui('manage-monitor').checked=false;ui('manage-monitor').dispatchEvent(new window.Event('change',{bubbles:true}));assertStopped()});
  await check('visibility change clears polling and checkbox',()=>{enable();hidden=true;document.dispatchEvent(new window.Event('visibilitychange'));assertStopped();hidden=false});
  await check('window blur clears polling and checkbox',()=>{enable();window.dispatchEvent(new window.Event('blur'));assertStopped()});
  await check('page departure clears polling and checkbox',()=>{enable();window.dispatchEvent(new window.Event('pagehide'));assertStopped()});
  await check('hidden guard prevents requests even without a lifecycle event',async()=>{const tick=enable(),before=stateReads;hidden=true;tick();await flush();assertStopped();assert.equal(stateReads,before);hidden=false});
  await check('focus guard prevents requests even without a blur event',async()=>{const tick=enable(),before=stateReads;focused=false;tick();await flush();assertStopped();assert.equal(stateReads,before);focused=true});
  for(const failedRoute of ['bootstrap','state']){
   await check('initial '+failedRoute+' failure shows an honest recoverable connection state',async()=>{
    let failing=true,requests=0;
    const isolated=new JSDOM(html,{url:'http://127.0.0.1:3001/',runScripts:'dangerously',pretendToBeVisual:true,virtualConsole:consoleSink,beforeParse(w){
     w.matchMedia=query=>({matches:false,media:query,addEventListener(){},removeEventListener(){}});
     w.HTMLElement.prototype.scrollIntoView=function(){};
     w.fetch=async url=>{
      requests++;
      if(failing&&url==='/api/navigation/'+failedRoute)throw new TypeError('Failed to fetch');
      if(url==='/api/navigation/bootstrap')return {ok:true,json:async()=>({token:'test-session'})};
      assert.equal(url,'/api/navigation/state','recovery must never mutate the nodes');
      return {ok:true,json:async()=>({result:window.navigationSnapshot})};
     };
    }});
    try{
     await flush();const d=isolated.window.document,get=id=>d.getElementById(id);
     assert.doesNotMatch(get('manage-connection').textContent,/正在|已连接/);
     assert.match(get('manage-connection').textContent,/失败|不可用|未连接/);
     assert.doesNotMatch(get('manage-message').textContent,/Failed to fetch/);
     assert.doesNotMatch(get('manage-nodes').textContent,/正在读取|连接本机管理后端/);
     assert.match(get('manage-nodes').textContent,/未读取|不可用|失败/);
     assert.equal(get('manage-connect').hidden,false);assert.equal(get('manage-connect').disabled,false);
     assert.equal(get('manage-add').disabled,true);
     assert.ok(get('services').querySelectorAll('.card').length>0,'offline navigation remains usable');
     const previous=requests;failing=false;get('manage-connect').click();await flush();
     assert.ok(requests>previous);assert.equal(get('manage-connect').hidden,true);
     assert.equal(get('manage-add').disabled,false);
    }finally{isolated.window.close()}
   });
  }
  console.log(passed.length+' UI DOM checks passed; all APIs mocked, no live node requests issued');
 }finally{dom.window.close()}
})().catch(error=>{console.error(error);process.exitCode=1});
