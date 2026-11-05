#!/usr/bin/env python3
"""Three actual LAN-protocol daemons: membership, shared access and retained mail.

Isolated directories and controlled runtimes; no credentials or production state.
"""
import argparse,json,pathlib,secrets,socket,subprocess,tempfile,time,urllib.request,threading,struct
from smoke_helpers import fake_codex
ROOT=pathlib.Path(__file__).resolve().parent.parent
parser=argparse.ArgumentParser();parser.add_argument('artifact',nargs='?',type=pathlib.Path);parser.add_argument('--binary',type=pathlib.Path,default=ROOT/'target/debug/boxen');args=parser.parse_args()
binary=(args.artifact or args.binary).resolve();command=(['/bin/bash'] if binary.suffix=='.com' else [])+[str(binary)]
def port():
 with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
def wait(f,seconds=35):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
  if f():return
  time.sleep(.1)
 raise AssertionError('Network convergence deadline expired')
with tempfile.TemporaryDirectory(prefix='boxen-network-') as temp:
 homes=[pathlib.Path(temp)/str(i) for i in range(3)];ports=[port() for _ in homes];uis=[port() for _ in homes];ps=[None]*3;tokens=[None]*3
 def cli(i,*parts):return subprocess.check_output(command+['--data-dir',str(homes[i]),*parts],text=True,timeout=15).strip()
 keys=[cli(i,'identity') for i in range(3)]
 for a,b in [(0,1),(1,0),(1,2),(2,1)]:cli(a,'trust',keys[b],'Fixture machine '+str(b))
 def launch(i):
  ps[i]=subprocess.Popen(command+['--data-dir',str(homes[i]),'serve','--listen',f'127.0.0.1:{ports[i]}','--advertise',f'127.0.0.1:{ports[i]}','--ui',f'127.0.0.1:{uis[i]}','--codex',str(fake_codex())],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
  lines=[ps[i].stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1],(lines,ps[i].stderr.read(2048))
  tokens[i]=(homes[i]/'control-token').read_text().strip()
 def api(i,j,request):
  data={'peer':keys[j],'address':f'127.0.0.1:{ports[j]}','request':request}
  r=urllib.request.Request(f'http://127.0.0.1:{uis[i]}/api/operation',headers={'Authorization':'Bearer '+tokens[i],'Content-Type':'application/json'},data=json.dumps(data).encode())
  return json.load(urllib.request.urlopen(r,timeout=20))
 def peers(i):return json.loads(cli(i,'peers'))
 try:
  for i in range(3):launch(i)
  # One real initial connection discovers the endpoint in each original pairing.
  cli(0,'ping',f'127.0.0.1:{ports[1]}',keys[1],'fixture')
  cli(1,'ping',f'127.0.0.1:{ports[2]}',keys[2],'fixture')
  wait(lambda:all(len(peers(i))==2 for i in range(3)))
  assert len(api(0,0,{'op':'network','args':{'action':'status'}})['members'])==3
  assert len(api(0,2,{'op':'network','args':{'action':'status'}})['members'])==3
  assert json.loads(cli(0,'ping',f'127.0.0.1:{ports[2]}',keys[2],'introduced fixture'))['message']=='introduced fixture'
  other=secrets.token_hex(32)
  api(2,2,{'op':'resident_create','spec':{'id':other,'name':'Revocation fixture','cwd':temp,'model':'fixture','auto_activate':False}})
  resident=secrets.token_hex(32);run=secrets.token_hex(32)
  created=api(1,1,{'op':'resident_create','spec':{'id':resident,'name':'Host-local fixture','cwd':temp,'model':'fixture','auto_activate':False}})
  assert created['owner']==keys[1]
  assert any(r['id']==resident for r in api(0,1,{'op':'resident_list'}))
  got=api(2,1,{'op':'resident_get','id':resident});assert got['owner']==keys[1]
  updated=api(0,1,{'op':'resident_update','id':resident,'patch':{'expected_revision':got['revision'],'memory':'Shared operator update'}})
  assert updated['owner']==keys[1] and updated['memory']=='Shared operator update'
  api(1,1,{'op':'start','spec':{'id':run,'thread_id':run,'backend':'codex','cwd':temp,'model':'fixture','prompt':'Shared workload fixture'}})
  wait(lambda:api(0,1,{'op':'get','id':run})['status'] in ('completed','failed','interrupted','cancelled'))
  assert any(r['record']['spec']['id']==run for r in api(2,1,{'op':'list'}))
  request=urllib.request.Request(f'http://127.0.0.1:{uis[0]}/api/watch',headers={'Authorization':'Bearer '+tokens[0],'Content-Type':'application/json'},data=json.dumps({'peer':keys[1],'address':f'127.0.0.1:{ports[1]}','request':{'op':'watch','id':run,'after':0}}).encode())
  with urllib.request.urlopen(request,timeout=20) as response:
   batch=json.loads(response.readline());assert batch['events'] and batch['snapshot']['record']['owner']==keys[1]
  wait(lambda:any(resident in p.read_text() for p in (homes[0]/'network-directory').glob('*.json')))
  ps[1].terminate();ps[1].wait(timeout=10)
  message_id=secrets.token_hex(32);client=secrets.token_hex(32)
  mail={'op':'mailbox_send','client_id':client,'message':{'id':message_id,'to':{'type':'resident','id':resident},'subject':'Cross-machine fixture','body':'REPLY across hosts'}}
  queued=api(0,0,mail);assert queued['delivery']['status']=='queued' and queued['accepted_on']==keys[0]
  assert api(0,0,mail)['id']==message_id
  try:
   api(0,0,mail|{'message':mail['message']|{'body':'conflicting retry'}})
   raise AssertionError('Changed accepted mail was allowed')
  except urllib.error.HTTPError as error:assert error.code==422
  journal=homes[1]/'mailboxes/events.jsonl';saved=journal.with_name('original.jsonl');journal.rename(saved);journal.mkdir()
  launch(1)
  wait(lambda:bool(api(0,0,{'op':'messaging','args':{'action':'status','id':message_id}})['delivery'].get('last_error')),35)
  assert api(0,0,{'op':'messaging','args':{'action':'status','id':message_id}})['delivery']['status']=='queued'
  assert api(0,1,{'op':'mailbox_status'})['available']==False
  ps[1].terminate();ps[1].wait(timeout=10);journal.rmdir();saved.rename(journal)
  ps[0].terminate();ps[0].wait(timeout=10)
  # Lose the real destination acknowledgment after its durable mailbox intake.
  # Noise stays end-to-end; this proxy drops the second server frame (the first
  # is the handshake). The next source retry must reconcile the same message.
  launch(1)
  proxy=socket.socket();proxy.bind(('127.0.0.1',0));proxy.listen(1);proxy_port=proxy.getsockname()[1]
  def lose_ack():
   front,_=proxy.accept();back=socket.create_connection(('127.0.0.1',ports[1]));front.settimeout(15);back.settimeout(15)
   def upstream():
    try:
     while data:=front.recv(65536):back.sendall(data)
    except OSError:pass
   thread=threading.Thread(target=upstream,daemon=True);thread.start()
   def exact(n):
    out=b''
    while len(out)<n:
     chunk=back.recv(n-len(out))
     if not chunk:raise EOFError()
     out+=chunk
    return out
   try:
    header=exact(2);front.sendall(header+exact(struct.unpack('>H',header)[0]))
    header=exact(2);exact(struct.unpack('>H',header)[0])
   finally:
    front.close();back.close();proxy.close()
  dropped=threading.Thread(target=lose_ack,daemon=True);dropped.start()
  lost=subprocess.run(command+['--data-dir',str(homes[0]),'request',f'127.0.0.1:{proxy_port}',keys[1],json.dumps({'op':'network_mail','message':queued})],capture_output=True,text=True,timeout=20)
  dropped.join(timeout=20);assert lost.returncode!=0 and not dropped.is_alive()
  assert len(api(2,1,{'op':'resident_inbox','id':resident})['messages'])==1
  launch(0)
  wait(lambda:api(0,0,{'op':'messaging','args':{'action':'status','id':message_id}})['delivery']['status']=='accepted',60)
  incoming=api(2,1,{'op':'resident_inbox','id':resident})['messages'];assert len(incoming)==1 and incoming[0]['id']==message_id
  assert incoming[0]['sender']=={'type':'client','machine':keys[0],'id':client}
  activation=api(2,1,{'op':'resident_activate','id':resident,'activation_id':secrets.token_hex(32)})
  wait(lambda:api(2,1,{'op':'get','id':activation['record']['spec']['id']})['status']=='completed')
  wait(lambda:bool(api(0,0,{'op':'client_inbox','client_id':client})['messages']),60)
  replies=api(0,0,{'op':'client_inbox','client_id':client})['messages']
  assert len(replies)==1 and replies[0]['body']=='DELIVERED_EXPLICIT_REPLY' and replies[0]['in_reply_to']==message_id
  assert api(0,1,{'op':'client_inbox','client_id':client})['messages']==[],'Source outgoing copy appeared as incoming mail'
  # A resident's real runtime tool sends to a resident on another host. Receiving
  # that mail and completing without a reply remain separate operations.
  wait(lambda:any(other in p.read_text() for p in (homes[1]/'network-directory').glob('*.json')))
  api(0,1,{'op':'mailbox_send','client_id':client,'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':resident},'subject':'Relay fixture','body':'RELAY:'+other}})
  relay_run=api(0,1,{'op':'resident_activate','id':resident,'activation_id':secrets.token_hex(32)})
  wait(lambda:api(0,1,{'op':'get','id':relay_run['record']['spec']['id']})['status']=='completed')
  wait(lambda:bool(api(2,2,{'op':'resident_inbox','id':other})['messages']),60)
  peer_mail=api(2,2,{'op':'resident_inbox','id':other})['messages'];assert len(peer_mail)==1 and peer_mail[0]['sender']=={'type':'resident','id':resident}
  silent=api(0,2,{'op':'resident_activate','id':other,'activation_id':secrets.token_hex(32)})
  wait(lambda:api(0,2,{'op':'get','id':silent['record']['spec']['id']})['status']=='completed')
  assert len(api(0,0,{'op':'client_inbox','client_id':client})['messages'])==1,'Private completion became a reply'
  # Removal is propagated through the remaining member, retained after restart,
  # and cannot be undone by an offline member's old trust records.
  wait(lambda:any(other in p.read_text() for p in (homes[0]/'network-directory').glob('*.json')))
  ps[2].terminate();ps[2].wait(timeout=10)
  for n in range(256):
   api(0,0,{'op':'mailbox_send','client_id':client,'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':other},'subject':'Queued fixture','body':str(n)}})
  assert api(0,0,{'op':'mailbox_status'})['network_pending']==256
  try:
   api(0,0,{'op':'mailbox_send','client_id':client,'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':other},'subject':'Excess fixture','body':'not accepted'}})
   raise AssertionError('Pending network mail exceeded its bound')
  except urllib.error.HTTPError as error:assert error.code==422
  api(0,0,{'op':'network','args':{'action':'remove','key':keys[2]}});wait(lambda:len(peers(1))==1)
  ps[0].terminate();ps[0].wait(timeout=10);launch(0)
  assert len(peers(0))==1
  launch(2)
  probe=subprocess.run(command+['--data-dir',str(homes[2]),'ping',f'127.0.0.1:{ports[1]}',keys[1],'removed fixture'],capture_output=True,text=True,timeout=15)
  assert probe.returncode!=0,'Removed machine regained access'
  assert len(peers(0))==1 and len(peers(1))==1
  assert len(api(2,2,{'op':'resident_inbox','id':other})['messages'])==1
  assert api(0,0,{'op':'mailbox_status'})['network_pending']==256
  print(json.dumps({'membership_converged':True,'introduced_connection':True,'removal_converged':True,'restart_retained_removal':True,'stale_member_rejected':True,'pending_bound_and_revocation':True,'cross_owner_discovery_update_replay':True,'offline_mail_restart':True,'destination_storage_failure_retained_queue':True,'conflicting_mail_retry_rejected':True,'source_outgoing_hidden':True,'lost_destination_ack_deduplicated':True,'cross_machine_explicit_reply':True,'resident_relay_and_no_reply':True}))
 finally:
  for p in ps:
   if p and p.poll() is None:p.terminate()
  for p in ps:
   if p:
    try:p.wait(timeout=10)
    except subprocess.TimeoutExpired:p.kill();p.wait()
