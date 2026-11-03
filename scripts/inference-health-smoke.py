#!/usr/bin/env python3
"""Paired daemon text import: exact roles/provenance, no inference/effect replay, real next-input projection."""
import concurrent.futures
import http.server
import json
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
ROOT=pathlib.Path(__file__).resolve().parent.parent
BINARY=pathlib.Path(sys.argv[1] if len(sys.argv)>1 else ROOT/'target/release/boxen').resolve()
COMMAND=(['/bin/bash'] if BINARY.suffix=='.com' else [])+[str(BINARY)]
observed=[];mode='payment'
class Provider(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_POST(self):
  value=json.loads(self.rfile.read(int(self.headers['Content-Length'])));observed.append(value)
  messages=value['messages'];tool_results=[m for m in messages if m['role']=='tool']
  if mode=='payment' or (mode=='effect_then_payment' and tool_results):
   self.send_response(402);self.end_headers();self.wfile.write(b'{"error":{"message":"fixture billing"}}');return
  self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
  def emit(delta,finish=None):self.wfile.write(('data: '+json.dumps({'id':'fixture','choices':[{'index':0,'delta':delta,'finish_reason':finish}]})+'\n\n').encode());self.wfile.flush()
  if mode=='effect_then_payment':
   emit({'tool_calls':[{'index':0,'id':'effect-once','type':'function','function':{'name':'boxen_shell','arguments':json.dumps({'command':'printf x >> effect.txt'})}}]});emit({},'tool_calls')
  else:emit({'content':'PRIVATE_RESUMED'});emit({},'stop')
  self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()
def port():
 with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
with tempfile.TemporaryDirectory(prefix='boxen-inference-health-') as temp:
 base=pathlib.Path(temp);homes=[base/'a',base/'b'];peers=[port(),port()];uis=[port(),port()];processes=[None,None];tokens=[None,None]
 provider=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=provider.serve_forever,daemon=True).start()
 def cli(i,*args):return subprocess.check_output(COMMAND+['--data-dir',str(homes[i]),*args],text=True,timeout=20).strip()
 keys=[cli(i,'identity') for i in range(2)]
 for i in range(2):cli(i,'trust',keys[1-i],'Transcript test peer')
 def daemon(i):
  p=subprocess.Popen(COMMAND+['--data-dir',str(homes[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}','--codex',str(ROOT/'tests/fake-codex.py'),'--openrouter-endpoint',f'http://127.0.0.1:{provider.server_port}/chat'],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
  processes[i]=p;lines=[p.stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1];tokens[i]=lines[-1].strip().split('#')[1]
 def api(request,i=0):
  r=urllib.request.Request(f'http://127.0.0.1:{uis[i]}/api/operation',headers={'Authorization':'Bearer '+tokens[i],'Content-Type':'application/json'},data=json.dumps({'peer':keys[1],'address':f'127.0.0.1:{peers[1]}','request':request}).encode());return json.load(urllib.request.urlopen(r,timeout=20))
 def reject(fn):
  try:fn()
  except urllib.error.HTTPError as e:assert e.code==422;return
  raise AssertionError('operation was not rejected')
 def create(backend='openrouter'):
  return api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'name':'Text import fixture','backend':backend,'cwd':temp,'model':'current-model','identity':'A small exact identity.','auto_activate':False}})
 def import_(spec):return api({'op':'transcript_import','spec':spec})
 try:
  daemon(0);daemon(1)
  resident=create();rid=resident['id']
  def wait(snapshot):
   ident=snapshot['record']['spec']['id'];deadline=time.monotonic()+20
   while True:
    s=api({'op':'get','id':ident})
    if s['status'] in ['completed','interrupted','failed','cancelled']:return s
    assert time.monotonic()<deadline;time.sleep(.05)
  def activate():return wait(api({'op':'resident_activate','id':rid,'activation_id':secrets.token_hex(32),'directions':'An original request.'}))
  def health():return api({'op':'inference_health'})['incidents']
  first=activate();assert first['status']=='interrupted'
  h=health();assert len(h)==1 and h[0]['category']=='payment' and h[0]['http_status']==402 and h[0]['count']==1 and h[0]['active']
  assert api({'op':'inference_health'},1)['incidents']==[]
  reject(lambda:api({'op':'inference_resume','failed_run_id':first['record']['spec']['id'],'id':secrets.token_hex(32)},1))
  ident=secrets.token_hex(32);request={'op':'inference_resume','failed_run_id':first['record']['spec']['id'],'id':ident}
  second=wait(api(request));assert second['status']=='interrupted' and second['record']['spec']['thread_id']==rid
  assert api(request)==second
  h=health();assert len(h)==1 and h[0]['count']==2 and h[0]['first_at']<=h[0]['last_at'] and h[0]['active']
  reject(lambda:api(request|{'id':secrets.token_hex(32)}))
  processes[1].terminate();processes[1].wait(timeout=10);daemon(1)
  assert health()==h and api(request)==second
  mode='effect_then_payment'
  third=wait(api({'op':'inference_resume','failed_run_id':ident,'id':secrets.token_hex(32)}));assert third['status']=='interrupted'
  assert (base/'effect.txt').read_text()=='x'
  assert health()[0]['count']==3
  mode='success';newid=secrets.token_hex(32);latest=third['record']['spec']['id']
  success_request={'op':'inference_resume','failed_run_id':latest,'id':newid}
  fourth=wait(api(success_request));assert fourth['status']=='completed'
  assert api(success_request)==fourth and (base/'effect.txt').read_text()=='x'
  h=health();assert h[0]['count']==3 and not h[0]['active'] and h[0]['resolved_run_id']==newid
  wire=observed[-1]['messages'];system=wire[0]['content'];assert 'Inference health is a coalesced projection' in system and 'payment' in system and '"count":3' in system
  assert len([m for m in wire if m['role']=='tool'])==1
  assert wire[-1]['role']=='user' and 'Continuation after interrupted inference' in wire[-1]['content']
  assert api({'op':'resident_inbox','id':rid})['messages']==[]
  reject(lambda:api({'op':'inference_resume','failed_run_id':newid,'id':secrets.token_hex(32)}))
  assert json.loads(cli(0,'inference-health',f'127.0.0.1:{peers[1]}',keys[1]))['incidents']==h
  # Anonymous request/response conversations use the same continuation authority.
  mode='payment';anonymous=wait(api({'op':'start','spec':{'id':secrets.token_hex(32),'thread_id':secrets.token_hex(32),'backend':'openrouter','cwd':temp,'model':'current-model','prompt':'Ordinary request.'}}));assert anonymous['status']=='interrupted'
  mode='success';anon_id=secrets.token_hex(32)
  resumed=json.loads(cli(0,'inference-resume',f'127.0.0.1:{peers[1]}',keys[1],anonymous['record']['spec']['id'],'--id',anon_id));assert wait(resumed)['status']=='completed'
  assert api({'op':'thread_result','id':anon_id})['response']=='PRIVATE_RESUMED'
  print('PASS payment classification, owner isolation, bounded/coalesced projection, restart, exact-ID continuation and stale rejection; successful resolution; source-qualified next-input notice; completed tools not replayed; CLI and ordinary request/response recovery')
 finally:
  provider.shutdown();provider.server_close()
  for p in processes:
   if p and p.poll() is None:p.terminate()
  for p in processes:
   if p:
    try:p.wait(timeout=5)
    except subprocess.TimeoutExpired:p.kill();p.wait()
