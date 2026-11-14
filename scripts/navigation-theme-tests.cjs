#!/usr/bin/env node
// Exercises the shipped theme scripts without contacting any node or reading credentials.
'use strict';
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const {JSDOM,VirtualConsole}=require(process.argv[2]||'jsdom');
const html=fs.readFileSync(path.join(__dirname,'../service-navigation.html'),'utf8');
const storageKey='veytrane-navigation-theme';
const head=html.match(/<head>([\s\S]*?)<\/head>/)[1];
const header=html.match(/<header\b[\s\S]*?<\/header>/)[0];
const controls=html.match(/<script\b[^>]*id="theme-controls"[^>]*>[\s\S]*?<\/script>/)?.[0]||'';
assert.match(head,/id="theme-init"/,'The saved theme must be applied in the head before the body is rendered');
const markup='<!doctype html><html><head>'+head+
 '<script>window.themeBeforeBody=document.documentElement.dataset.theme;</script></head><body>'+
 header+controls+'</body></html>';
const scenarios=[];
async function scenario(name,options,verify){
 const errors=[],listeners=new Set();
 let dark=!!options.dark;
 const sink=new VirtualConsole();
 sink.on('jsdomError',error=>{if(error.type==='unhandled-exception'||/Uncaught/.test(error.message))errors.push(error)});
 const dom=new JSDOM(markup,{url:'http://127.0.0.1:3001/',runScripts:'dangerously',virtualConsole:sink,beforeParse(window){
  window.matchMedia=query=>({
   media:query,get matches(){return query.includes('prefers-color-scheme')&&dark},
   addEventListener(type,callback){if(type==='change')listeners.add(callback)},
   removeEventListener(type,callback){listeners.delete(callback)},
   addListener(callback){listeners.add(callback)},removeListener(callback){listeners.delete(callback)}
  });
  if(options.stored!==undefined)window.localStorage.setItem(storageKey,options.stored);
  if(options.blocked)Object.defineProperty(window,'localStorage',{get(){throw new window.DOMException('Storage unavailable','SecurityError')}});
 }});
 const {window}=dom;
 function systemTheme(value){
  dark=value;
  for(const callback of listeners)callback({matches:dark,media:'(prefers-color-scheme: dark)'});
 }
 try{
  await new Promise(resolve=>setImmediate(resolve));
  await verify(window,systemTheme);
  assert.deepEqual(errors,[]);
  scenarios.push(name);console.log('PASS '+name);
 }finally{dom.window.close()}
}
(async()=>{
 await scenario('uses the system theme before rendering and allows switching in both directions',{dark:true},window=>{
  const button=window.document.getElementById('theme-toggle'),root=window.document.documentElement;
  assert.equal(window.themeBeforeBody,'dark');assert.equal(root.dataset.theme,'dark');
  assert.equal(button.getAttribute('aria-pressed'),'true');assert.match(button.getAttribute('aria-label'),/亮色/);
  button.click();assert.equal(root.dataset.theme,'light');
  assert.equal(window.localStorage.getItem(storageKey),'light');
  assert.equal(button.getAttribute('aria-pressed'),'false');assert.match(button.getAttribute('aria-label'),/暗色/);
  for(const token of ['--bg','--ink','--muted','--line','--blue','--green','--red']){
   assert.ok(window.getComputedStyle(root).getPropertyValue(token).trim(),'Shared CSS token must remain available in light mode: '+token);
  }
  button.click();assert.equal(root.dataset.theme,'dark');assert.equal(window.localStorage.getItem(storageKey),'dark');
 });
 await scenario('restores saved dark preference despite a light system setting',{stored:'dark'},window=>{
  assert.equal(window.themeBeforeBody,'dark');
  assert.equal(window.document.getElementById('theme-toggle').getAttribute('aria-pressed'),'true');
 });
 await scenario('restores saved light preference despite a dark system setting',{stored:'light',dark:true},window=>{
  assert.equal(window.themeBeforeBody,'light');
  assert.equal(window.document.getElementById('theme-toggle').getAttribute('aria-pressed'),'false');
 });
 await scenario('ignores invalid stored values',{stored:'unexpected',dark:true},window=>{
  assert.equal(window.themeBeforeBody,'dark');
 });
 await scenario('system changes apply until the user makes an explicit choice',{},(window,systemTheme)=>{
  const root=window.document.documentElement;
  assert.equal(root.dataset.theme,'light');systemTheme(true);assert.equal(root.dataset.theme,'dark');
  window.document.getElementById('theme-toggle').click();assert.equal(root.dataset.theme,'light');
  systemTheme(false);systemTheme(true);assert.equal(root.dataset.theme,'light');
 });
 await scenario('switches normally when browser storage is unavailable',{blocked:true,dark:true},window=>{
  const root=window.document.documentElement,button=window.document.getElementById('theme-toggle');
  assert.equal(root.dataset.theme,'dark');button.click();assert.equal(root.dataset.theme,'light');button.click();assert.equal(root.dataset.theme,'dark');
 });
 console.log(scenarios.length+' theme checks passed; no network requests issued');
})().catch(error=>{console.error(error);process.exitCode=1});
