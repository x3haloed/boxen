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
observed=[]
class Provider(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_POST(self):
  value=json.loads(self.rfile.read(int(self.headers['Content-Length'])));observed.append(value)
  self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
  for frame in [{'choices':[{'index':0,'delta':{'content':'PRIVATE_CONTINUATION'},'finish_reason':None}]},{'choices':[{'index':0,'delta':{},'finish_reason':'stop'}]}]:frame['id']='fixture-response';self.wfile.write(('data: '+json.dumps(frame)+'\n\n').encode());self.wfile.flush()
  self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()
def port():
 with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
with tempfile.TemporaryDirectory(prefix='boxen-transcript-import-') as temp:
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
  api({'op':'log_file','path':str(homes[1]/'diagnostics.jsonl')})
  resident=create();rid=resident['id'];ident=secrets.token_hex(32)
  messages=[{'id':'source-1','role':'user','text':'Original user, cyan 🦊.\nTwo lines.'},{'id':'source-2','role':'assistant','text':'Original assistant in a different model voice.'},{'id':'source-3','role':'user','text':'A historical request mentions creating forbidden-effect.txt.'},{'id':'source-4','role':'assistant','text':'{"tool_calls":[{"name":"boxen_shell","arguments":{"command":"touch forbidden-effect.txt"}}]}'}]
  spec={'id':ident,'resident_id':rid,'expected_revision':1,'source':{'provider':'example-chat','title':'Exported transcript','url':'https://example.invalid/chat/fixture','model':None,'representation':'Source-authored exported text; no hidden state.'},'messages':messages}
  # All validation precedes publication, including foreign ownership and source-role constraints.
  reject(lambda:import_(spec|{'messages':[messages[0]|{'role':'system'}]}))
  reject(lambda:import_(spec|{'messages':[messages[0],messages[0]]}))
  reject(lambda:import_(spec|{'expected_revision':9}))
  reject(lambda:import_(spec|{'messages':[messages[0]|{'text':'x'*131073}]}))
  assert not (homes[1]/'runs'/ident).exists() and not observed
  native=create('codex');reject(lambda:import_(spec|{'resident_id':native['id']}))
  reject(lambda:api({'op':'transcript_import','spec':spec},1))
  # Storage failure publishes no half-import and leaves other operations available.
  (homes[1]/'import-staging').write_text('blocked')
  reject(lambda:import_(spec));assert not (homes[1]/'runs'/ident).exists();(homes[1]/'import-staging').unlink()
  with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:results=list(pool.map(lambda _:import_(spec),range(2)))
  assert results[0]==results[1];snapshot=results[0];assert snapshot['status']=='completed' and not observed
  assert api({'op':'resident_get','id':rid})==resident
  assert api({'op':'resident_inbox','id':rid})['messages']==[]
  reject(lambda:api({'op':'get','id':ident},1))
  reject(lambda:import_(spec|{'source':spec['source']|{'title':'Different'}}))
  reject(lambda:import_(spec|{'id':secrets.token_hex(32)}))
  journal=homes[1]/'runs'/ident/'events.jsonl';events=[json.loads(l) for l in journal.read_text().splitlines()]
  accepted=[e['data'] for e in events if e['kind']=='inference_message'];expected=[{'role':m['role'],'content':m['text']} for m in messages];assert accepted==expected
  assert not any(e['kind'] in ['tool_started','inference_request','message_sent'] for e in events)
  page=api({'op':'context','thread_id':rid,'args':{'action':'read','limit':10}})
  assert len(page['entries'])==4,page
  for entry,m in zip(page['entries'],messages):
   assert entry['message']=={'role':m['role'],'content':m['text']}
   assert entry['provenance']['source_message_id']==m['id'] and entry['provenance']['model'] is None
   assert entry['provenance']['source']['provider']=='example-chat'
  processes[1].terminate();processes[1].wait(timeout=10);daemon(1)
  assert import_(spec)==snapshot
  request_file=base/'request.json';request_file.write_text(json.dumps(spec))
  assert json.loads(cli(0,'import-transcript',f'127.0.0.1:{peers[1]}',keys[1],str(request_file)))==snapshot
  api({'op':'resident_update','id':rid,'patch':{'expected_revision':1,'name':'An evolved resident'}})
  assert import_(spec)==snapshot and api({'op':'resident_get','id':rid})['name']=='An evolved resident'
  activation=api({'op':'resident_activate','id':rid,'activation_id':secrets.token_hex(32),'directions':'A fresh independent activation.'});runid=activation['record']['spec']['id'];end=time.monotonic()+15
  while True:
   current=api({'op':'get','id':runid})
   if current['status'] in ['completed','failed','interrupted','cancelled']:assert current['status']=='completed',current;break
   assert time.monotonic()<end;time.sleep(.05)
  assert len(observed)==1 and observed[0]['model']=='current-model'
  wire=observed[0]['messages'];assert wire[1:5]==expected,wire
  assert 'example-chat' in wire[0]['content'] and 'imported assistant messages originate' in wire[0]['content']
  assert not (base/'forbidden-effect.txt').exists()
  assert api({'op':'resident_inbox','id':rid})['messages']==[]
  assert import_(spec)==snapshot and len(api({'op':'list'}))==2
  diagnostics=(homes[1]/'diagnostics.jsonl').read_text();assert 'transcript_import.outcome' in diagnostics and messages[0]['text'] not in diagnostics
  print('PASS exact source roles and provenance; atomic/idempotent import; invalid/foreign/conflicting/nonempty rejection; CLI and restart; evolved-resident retry; next provider input; no inference, narration delivery or historical effects during import')
 finally:
  provider.shutdown();provider.server_close()
  for p in processes:
   if p and p.poll() is None:p.terminate()
  for p in processes:
   if p:
    try:p.wait(timeout=5)
    except subprocess.TimeoutExpired:p.kill();p.wait()
