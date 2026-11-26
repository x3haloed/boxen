#!/usr/bin/env python3
"""Paired daemons, retained inference incidents, payment cooldown, and continuation."""
import http.server
from smoke_helpers import fake_codex
from smoke_helpers import python_shell
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
  self.send_response(200)
  self.send_header('Content-Type','text/html' if mode=='wrong_content_type' else 'text/event-stream')
  self.end_headers()
  if mode=='wrong_content_type':return
  if mode=='malformed_frame':
   self.wfile.write(b'data: fixture private non-JSON payload\n\n');return
  if mode=='truncated_stream':
   self.wfile.write(b'data: {"choices":[{"index":0,"delta":{"content":"partial"}}]}\n\n');return
  if mode=='unsupported_projection':
   self.wfile.write(b'data: {"choices":[{"index":1,"delta":{"content":"other"},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');return
  def emit(delta,finish=None):self.wfile.write(('data: '+json.dumps({'id':'fixture','choices':[{'index':0,'delta':delta,'finish_reason':finish}]})+'\n\n').encode());self.wfile.flush()
  if mode=='effect_then_payment':
   emit({'tool_calls':[{'index':0,'id':'effect-once','type':'function','function':{'name':'boxen_shell','arguments':json.dumps({'command':python_shell("from pathlib import Path; p=Path('effect.txt'); p.open('a').write('x')")})}}]});emit({},'tool_calls')
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
 def daemon(i, endpoint_port=None):
  endpoint_port=provider.server_port if endpoint_port is None else endpoint_port
  p=subprocess.Popen(COMMAND+['--data-dir',str(homes[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}','--codex',str(fake_codex()),'--openrouter-endpoint',f'http://127.0.0.1:{endpoint_port}/chat'],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
  processes[i]=p;lines=[p.stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1];tokens[i]=lines[-1].strip().split('#')[1]
 def api(request,i=0):
  r=urllib.request.Request(f'http://127.0.0.1:{uis[i]}/api/operation',headers={'Authorization':'Bearer '+tokens[i],'Content-Type':'application/json'},data=json.dumps({'peer':keys[1],'address':f'127.0.0.1:{peers[1]}','request':request}).encode());return json.load(urllib.request.urlopen(r,timeout=20))
 def reject(fn):
  try:fn()
  except urllib.error.HTTPError as e:assert e.code==422;return
  raise AssertionError('operation was not rejected')
 def create(backend='openrouter',auto_activate=False):
  return api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'name':'Text import fixture','backend':backend,'cwd':temp,'model':'current-model','identity':'A small exact identity.','auto_activate':auto_activate}})
 try:
  daemon(0);daemon(1)
  resident=create();rid=resident['id']
  def wait(snapshot,seconds=20):
   ident=snapshot['record']['spec']['id'];deadline=time.monotonic()+seconds
   while True:
    s=api({'op':'get','id':ident})
    if s['status'] in ['completed','interrupted','failed','cancelled']:return s
    assert time.monotonic()<deadline;time.sleep(.05)
  def activate():return wait(api({'op':'resident_activate','id':rid,'activation_id':secrets.token_hex(32),'directions':'An original request.'}))
  def health():return api({'op':'inference_health'})['incidents']
  first=activate();assert first['status']=='interrupted'
  h=health();assert len(h)==1 and h[0]['category']=='payment' and h[0]['http_status']==402 and h[0]['count']==1 and h[0]['active']
  assert (h[0]['failure_stage'],h[0]['failure_code'])==('http','http_rejected')
  assert 'fixture billing' not in json.dumps(h)
  assert api({'op':'inference_health'},1)['incidents']==h
  ident=secrets.token_hex(32);request={'op':'inference_resume','failed_run_id':first['record']['spec']['id'],'id':ident}
  second=wait(api(request));assert second['status']=='interrupted' and second['record']['spec']['thread_id']==rid
  assert api(request)==second and api(request,1)==second
  h=health();assert len(h)==1 and h[0]['count']==2 and h[0]['first_at']<=h[0]['last_at'] and h[0]['active']
  reject(lambda:api(request|{'id':secrets.token_hex(32)}))
  processes[1].terminate();processes[1].wait(timeout=10);daemon(1)
  assert health()==h and api(request)==second
  assert h[0]['automatic_cooldown_until_ms']>time.time_ns()//1_000_000
  mode='effect_then_payment'
  third=wait(api({'op':'inference_resume','failed_run_id':ident,'id':secrets.token_hex(32)}));assert third['status']=='interrupted'
  assert (base/'effect.txt').read_text()=='x'
  assert health()[0]['count']==3
  mode='success';newid=secrets.token_hex(32);latest=third['record']['spec']['id']
  success_request={'op':'inference_resume','failed_run_id':latest,'id':newid}
  fourth=wait(api(success_request));assert fourth['status']=='completed'
  assert api(success_request)==fourth and (base/'effect.txt').read_text()=='x'
  h=health();assert h[0]['count']==3 and not h[0]['active'] and h[0]['resolved_run_id']==newid
  wire=observed[-1]['messages'];system=wire[-1]['content'];assert 'Inference health is a coalesced projection' in system and 'payment' in system and '"count":3' in system
  assert len([m for m in wire if m['role']=='tool'])==1
  assert wire[-2]['role']=='user' and 'Continuation after interrupted inference' in wire[-2]['content']
  assert api({'op':'resident_inbox','id':rid})['messages']==[]
  reject(lambda:api({'op':'inference_resume','failed_run_id':newid,'id':secrets.token_hex(32)}))
  assert json.loads(cli(0,'inference-health',f'127.0.0.1:{peers[1]}',keys[1]))['incidents']==h
  # Anonymous request/response conversations use the same continuation authority.
  mode='payment';anonymous=wait(api({'op':'start','spec':{'id':secrets.token_hex(32),'thread_id':secrets.token_hex(32),'backend':'openrouter','cwd':temp,'model':'current-model','prompt':'Ordinary request.'}}));assert anonymous['status']=='interrupted'
  mode='success';anon_id=secrets.token_hex(32)
  resumed=json.loads(cli(0,'inference-resume',f'127.0.0.1:{peers[1]}',keys[1],anonymous['record']['spec']['id'],'--id',anon_id));assert wait(resumed)['status']=='completed'
  assert api({'op':'thread_result','id':anon_id})['response']=='PRIVATE_RESUMED'
  # Automatic mail, schedule and observation delivery share the retained cooldown.
  auto=create(auto_activate=True)['id']
  def runs(thread):
   return [r for r in api({'op':'list'}) if r['record']['spec']['thread_id']==thread]
  def until(check,seconds=15):
   deadline=time.monotonic()+seconds
   while time.monotonic()<deadline:
    result=check()
    if result:return result
    time.sleep(.1)
   raise AssertionError('Condition did not become true before deadline')
  def mail(thread,body):
   return api({'op':'mailbox_send','client_id':keys[0],'message':
    {'id':secrets.token_hex(32),'to':{'type':'resident','id':thread},
     'subject':'Cooldown fixture','body':body}})['id']
  def inbox(thread):
   return {m['id']:m for m in api({'op':'resident_inbox','id':thread})['messages']}
  def now_ms():return time.time_ns()//1_000_000
  mode='payment'
  first_mail=[]
  for count in (1,2):
   first_mail.append(mail(auto,'Original activation '+str(count)))
   active=until(lambda:runs(auto) if len(runs(auto))==count else None)
   latest=max(active,key=lambda r:r['record']['created_at'])
   assert wait(latest)['status']=='interrupted'
  def payment():
   return next(row for row in health() if row['thread_id']==auto and row['category']=='payment')
  cooldown=payment()['automatic_cooldown_until_ms']
  assert payment()['count']==2 and cooldown>now_ms()
  pending=[mail(auto,'Queued during cooldown '+str(index)) for index in range(2)]
  assert [inbox(auto)[mid]['delivery'] for mid in first_mail+pending]==['presented']*2+['queued']*2
  schedule_id=secrets.token_hex(32)
  schedule={'id':schedule_id,'name':'Pending payment cooldown','target':{'type':'resident','id':auto},
            'directions':'Scheduled fresh work.','at_ms':now_ms()}
  api({'op':'schedule','args':{'action':'create','spec':schedule}})
  def scheduled():
   return api({'op':'schedule','args':{'action':'get','id':schedule_id}})
  until(lambda:scheduled().get('retry_at_ms')==cooldown)
  assert scheduled().get('claimed') is None
  def event(action,**args):return api({'op':'events','args':{'action':action,**args}})
  source_id=secrets.token_hex(32);subscription_id=secrets.token_hex(32)
  source=event('create_source',spec={'id':source_id,'name':'Cooldown observation','cwd':temp,
                'command':[],'interval_ms':1000,'mode':'ordered','timeout_ms':1000,'tools':[]})
  event('subscribe',spec={'id':subscription_id,'source':source_id,'target':{'type':'resident','id':auto},
                         'interval_ms':1000,'waking':True,'lifetime':'persistent'})
  event('publish',id=source_id,generation=source['generation'],sequence=1,
        response={'protocol':'boxen-adapter-v1','observations':[
          {'id':'fresh','observed_at_ms':now_ms(),'payload':{'proof':'recover-once'}}]})
  def subscription():return event('subscription',id=subscription_id)
  until(lambda:subscription()['cutoff']==1)
  assert subscription()['cursor']==0 and len(runs(auto))==2
  processes[1].terminate();processes[1].wait(timeout=10);daemon(1)
  assert payment()['automatic_cooldown_until_ms']==cooldown
  assert len(runs(auto))==2 and all(inbox(auto)[mid]['delivery']=='queued' for mid in pending)
  mode='success'
  unrelated=create(auto_activate=True)['id']
  mail(unrelated,'Unrelated resident remains available')
  assert wait(until(lambda:next(iter(runs(unrelated)),None)))['status']=='completed'
  assert now_ms()<cooldown and len(runs(auto))==2
  before=len(observed)
  recovered=until(lambda:next((r for r in runs(auto) if r['record'].get('scheduled')),None),
                  seconds=75)
  assert wait(recovered)['status']=='completed'
  assert recovered['record']['mailbox']['message_ids']==pending
  assert 'recover-once' in json.dumps(recovered['record']['observations'])
  assert subscription()['cursor']==1
  assert len(runs(auto))==3 and len(observed)==before+1
  assert not payment()['active'] and 'automatic_cooldown_until_ms' not in payment()
  assert all(inbox(auto)[mid]['delivery']=='presented' for mid in pending)
  print('PASS repeated HTTP 402 delays automatic mail, schedule and observation delivery across restart; unrelated resident runs and timer dispatches only fresh work',flush=True)
  def assert_failure(expected, http_status):
   thread=secrets.token_hex(32)
   failed=wait(api({'op':'start','spec':{'id':thread,'thread_id':thread,'backend':'openrouter',
       'cwd':temp,'model':'fixture','prompt':'Controlled failure stage.'}}),seconds=35)
   assert failed['status']=='interrupted'
   incident=next(row for row in health() if row['thread_id']==thread)
   actual=(incident['failure_stage'],incident['failure_code'])
   assert actual in expected and incident['http_status']==http_status,incident
   journal=homes[1]/'runs'/thread/'events.jsonl'
   failures=[e['data']['event'] for e in map(json.loads,journal.read_text().splitlines())
             if e['kind']=='inference_frame' and e['data']['event']['type']=='exchange_failed']
   assert len(failures)==1 and (failures[0]['stage'],failures[0]['code'])==actual
   assert 'fixture private non-JSON payload' not in json.dumps(failures)
  for mode, stage, code in [
   ('wrong_content_type','response_headers','unexpected_content_type'),
   ('malformed_frame','frame','invalid_json'),
   ('truncated_stream','stream','missing_done'),
   ('unsupported_projection','projection','unsupported_projection'),
  ]:
   assert_failure({(stage,code)},200)
  with socket.socket() as refused:
   refused.bind(('127.0.0.1',0))
   processes[1].terminate();processes[1].wait(timeout=10);daemon(1,refused.getsockname()[1])
   # A platform connector can exhaust its address budget after a refusal.
   assert_failure({('connection','connect_failed'),('timeout','connect_timeout')},None)
  print('PASS credential-free runtime failure evidence distinguishes connection, HTTP, headers, framing, stream termination and projection')
  print('PASS payment classification, shared operator access, bounded/coalesced projection, restart, exact-ID continuation and stale rejection; successful resolution; source-qualified next-input notice; completed tools not replayed; CLI and ordinary request/response recovery')
 finally:
  provider.shutdown();provider.server_close()
  for p in processes:
   if p and p.poll() is None:p.terminate()
  for p in processes:
   if p:
    try:p.wait(timeout=5)
    except subprocess.TimeoutExpired:p.kill();p.wait()
