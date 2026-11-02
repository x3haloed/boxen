#!/usr/bin/env python3
"""Real paired daemons, resident tools, isolated connector accounts and controlled Discord HTTP."""
import http.server
import json
import os
import pathlib
import secrets
import shutil
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
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]

class Discord(http.server.BaseHTTPRequestHandler):
    messages=[];sent=[];counter=100;rate_limit=False;unknown=False
    def log_message(self,*args):pass
    def reply(self,status,value):
        data=json.dumps(value).encode();self.send_response(status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def account(self):
        return {'Bot fixture-a':{'id':'1001','bot':True,'username':'First bot'},'Bot fixture-b':{'id':'1002','bot':True,'username':'Second bot'}}.get(self.headers.get('Authorization'))
    def do_GET(self):
        account=self.account()
        if not account:self.reply(401,{'error':'bad credential'});return
        if self.path=='/users/@me':self.reply(200,account);return
        assert self.path.startswith('/channels/999/messages?'),self.path
        before=int(self.path.split('before=')[1]) if 'before=' in self.path else 2**64
        self.reply(200,[m for m in reversed(Discord.messages) if int(m['id'])<before][:100])
    def do_POST(self):
        account=self.account();assert account
        body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert body['allowed_mentions']=={'parse':[]},body
        if Discord.rate_limit:Discord.rate_limit=False;self.reply(429,{'retry_after':0.01,'global':False});return
        value=add(body['content'],account);Discord.sent.append({'account':account['id'],'body':body,'id':value['id']})
        if Discord.unknown:Discord.unknown=False;self.connection.shutdown(socket.SHUT_RDWR);self.connection.close();return
        self.reply(200,value)
def add(body,author=None):
    Discord.counter+=1
    value={'id':str(Discord.counter),'channel_id':'999','content':body,'author':author or {'id':'2222','bot':False,'username':'Human fixture'},'timestamp':'2026-11-01T00:00:00Z','attachments':[]}
    Discord.messages.append(value);return value

with tempfile.TemporaryDirectory(prefix='boxen-messaging-') as temporary:
    directory=pathlib.Path(temporary);homes=[directory/'a',directory/'b'];peers=[port(),port()];uis=[port(),port()];processes=[]
    def cli(i,*args):return subprocess.check_output(COMMAND+['--data-dir',str(homes[i]),*args],text=True,timeout=15).strip()
    keys=[cli(i,'identity') for i in range(2)]
    for i in range(2):cli(i,'trust',keys[1-i],'Messaging fixture peer')
    tokens=[]
    def daemon(i):
        p=subprocess.Popen(COMMAND+['--data-dir',str(homes[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}','--codex',str(ROOT/'tests/fake-codex.py')],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
        processes.append(p);lines=[p.stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1],lines
        return p,lines[-1].strip().split('#')[1]
    for i in range(2):p,t=daemon(i);tokens.append(t)
    peer_process=p
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Discord);threading.Thread(target=server.serve_forever,daemon=True).start()
    def api(request,i=0):
        r=urllib.request.Request(f'http://127.0.0.1:{uis[i]}/api/operation',headers={'Authorization':'Bearer '+tokens[i],'Content-Type':'application/json'},data=json.dumps({'peer':keys[1],'address':f'127.0.0.1:{peers[1]}','request':request}).encode())
        return json.load(urllib.request.urlopen(r,timeout=20))
    def msg(action,i=0,**kwargs):return api({'op':'messaging','args':{'action':action,**kwargs}},i)
    def reject(call):
        try:call()
        except urllib.error.HTTPError as e:assert e.code==422,e.code;return
        raise AssertionError('operation should have been rejected')
    def resident(name):return api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'name':name,'cwd':temporary,'auto_activate':False}})['id']
    def inbox(r):return msg('inbox',conversation='resident:'+r)['messages']
    def instance(name,account,residents,key):
        credential=directory/(name+'.key');credential.write_text(key);credential.chmod(0o600)
        d={'id':secrets.token_hex(32),'name':name,'provider':'discord','account':account,'credential_file':str(credential),'capabilities':['send','receive','replies','native_metadata'],'bindings':[{'conversation':'999','resident':r} for r in residents]}
        value=msg('create',definition=d);assert msg('create',definition=d)==value
        return value
    def runner(instance,expected=0):
        r=subprocess.run(COMMAND+['--data-dir',str(homes[0]),'discord-connector','--instance',instance['definition']['id'],'--url',f'http://127.0.0.1:{uis[0]}','--peer',keys[1],'--address',f'127.0.0.1:{peers[1]}','--api-base',f'http://127.0.0.1:{server.server_port}','--once'],capture_output=True,text=True,timeout=30)
        assert r.returncode==expected,(r.returncode,r.stdout,r.stderr)
        assert 'fixture-a' not in r.stdout+r.stderr and 'fixture-b' not in r.stdout+r.stderr
        return r
    def activate(r,directions=''):
        value=api({'op':'resident_activate','id':r,'activation_id':secrets.token_hex(32),'directions':directions});assert value
        ident=value['record']['spec']['id']
        deadline=time.monotonic()+15
        while time.monotonic()<deadline:
            snapshot=api({'op':'get','id':ident})
            if snapshot['status'] in ('completed','failed','interrupted','cancelled'):
                assert snapshot['status']=='completed',snapshot
                return snapshot
            time.sleep(.04)
        raise AssertionError('resident did not complete')
    def command(r,instance,body='Connector reply',reply=None,conversation='999',subject=None):
        ident=secrets.token_hex(32)
        call={'action':'send','id':ident,'to':{'instance':instance['definition']['id'],'conversation':conversation},'body':body}
        if subject is not None:call['subject']=subject
        if reply:call['in_reply_to']=reply
        api({'op':'mailbox_send','client_id':keys[0],'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':r},'subject':'Controlled messaging tool call','body':'MESSAGING:'+json.dumps(call)}})
        activate(r);return ident
    try:
        log=directory/'diagnostics.jsonl';api({'op':'log_file','path':str(log)})
        r1,r2,r3=[resident(n) for n in ['First resident','Second resident','Explicitly shared bot resident']]
        a=instance('First connector','1001',[r1],'fixture-a');b=instance('Second connector','1002',[r2,r3],'fixture-b')
        aid,bid=a['definition']['id'],b['definition']['id']
        reject(lambda:msg('get',i=1,instance=aid))
        reject(lambda:msg('create',definition=a['definition']|{'account':'1002'}))
        runner(a);runner(b)
        native=add('SILENT native input')
        runner(a);runner(b)
        ia,ib,ic=inbox(r1),inbox(r2),inbox(r3)
        assert len(ia)==len(ib)==len(ic)==1
        assert ia[0]['id']!=ib[0]['id']!=ic[0]['id']
        assert ia[0]['origin']['account']=='1001' and ib[0]['origin']['account']=='1002'
        assert ia[0]['origin']['native_id']==ib[0]['origin']['native_id']==native['id']
        assert ia[0]['from']['instance']==aid and ib[0]['from']['instance']==bid
        assert ia[0]['origin']['author']['id']=='2222'
        local=command(r1,{'definition':{'id':'boxen'}},body='SILENT generic Boxen mail',conversation='resident:'+r2,subject='Local connector proof');assert any(m['id']==local and m['from']['instance']=='boxen' for m in inbox(r2))
        custom=msg('create',definition={'id':secrets.token_hex(32),'name':'Credential-free custom addon','provider':'custom','account':'public-room','capabilities':['receive','native_metadata'],'settings':{'endpoint':'example.invalid'},'bindings':[{'conversation':'a:b','resident':r3,'send':False},{'conversation':'a','resident':r3,'send':False}]})
        custom_id=custom['definition']['id']
        for conversation,native_id in [('a:b','c'),('a','b:c')]:
            msg('ingest',instance=custom_id,conversation=conversation,after=None,cursor='one',messages=[{'id':native_id,'author':{'id':'external-author'},'body':'SILENT opaque identifier collision proof'}])
        assert len([m for m in inbox(r3) if m.get('origin',{}).get('instance')==custom_id])==2
        activate(r1);assert not msg('outbox' ,instance=aid)['messages'],'private narration became an outgoing send'
        assert not Discord.sent
        bad_reply=command(r1,a,reply=ib[0]['id']);reject(lambda:msg('status',id=bad_reply))
        first=command(r1,a,reply=ia[0]['id']);assert msg('status',id=first)['delivery']['status']=='queued'
        Discord.rate_limit=True;runner(a)
        assert msg('status',id=first)['delivery']['status']=='accepted'
        assert Discord.sent[-1]['body']['message_reference']['message_id']==native['id']
        assert Discord.sent[-1]['account']=='1001' and len(Discord.sent)==1
        runner(a);assert len(Discord.sent)==1,'completed send replayed'
        # A resident's bindings scope the tool even when the other instance shares a room.
        denied=command(r1,b);reject(lambda:msg('status',id=denied))
        second=command(r2,b);runner(b);assert Discord.sent[-1]['account']=='1002'
        shared=command(r3,b);runner(b);assert Discord.sent[-1]['account']=='1002'
        assert msg('status',id=second)['delivery']['status']=='accepted' and msg('status',id=shared)['delivery']['status']=='accepted'
        # Replayed native input is accepted once, conflicting reuse is legible.
        checkpoint=msg('cursor',instance=aid,conversation='999')['checkpoint']['cursor']
        batch={'instance':aid,'conversation':'999','after':checkpoint,'cursor':'9000','messages':[{'id':'9000','author':{'id':'2222','bot':False},'body':'SILENT dedup proof'}]}
        msg('ingest',**batch);before=len(inbox(r1));assert msg('ingest',**batch)['replayed'];assert len(inbox(r1))==before
        reject(lambda:msg('ingest',**(batch|{'messages':[{'id':'9000','author':{'id':'2222'},'body':'changed'}]})))
        reject(lambda:msg('ingest',**(batch|{'cursor':'9001','after':'stale'})))
        msg('ingest',instance=aid,conversation='999',after='9000',cursor='9001',messages=[{'id':'9001','author':{'id':'2222','bot':False},'body':'x'*40000,'oversized_metadata':'y'*35000}])
        unavailable=inbox(r1)[-1];assert unavailable['body']=='' and unavailable['origin']['availability']['body']['retained'] is False and unavailable['origin']['availability']['metadata']['retained'] is False
        uncertain=command(r1,a,'Uncertain external effect');Discord.unknown=True;runner(a)
        assert msg('status',id=uncertain)['delivery']['status']=='uncertain'
        count=len(Discord.sent);runner(a);assert len(Discord.sent)==count
        peer_process.kill();peer_process.wait(timeout=5);peer_process,tokens[1]=daemon(1)
        assert msg('status',id=uncertain)['delivery']['status']=='uncertain';runner(a);assert len(Discord.sent)==count
        fresh=command(r1,a,'New work after uncertain send');runner(a);assert msg('status',id=fresh)['delivery']['status']=='accepted';assert len(Discord.sent)==count+1
        credential=pathlib.Path(a['definition']['credential_file']);credential.write_text('fixture-b');credential.chmod(0o600)
        runner(a,1);assert len(Discord.sent)==count+1,'wrong account sent as another connector'
        credential.write_text('fixture-a');credential.chmod(0o600);runner(a)
        paused=msg('update',instance=aid,expected_revision=1,enabled=False);assert paused['revision']==2
        reject(lambda:msg('update',instance=aid,expected_revision=1,enabled=True))
        runner(a);assert len(Discord.sent)==count+1
        msg('update',instance=aid,expected_revision=2,enabled=True)
        # One corrupt instance stays isolated from the other instance and ordinary workloads.
        (homes[1]/'connectors'/(bid+'.json')).write_text('{broken')
        peer_process.kill();peer_process.wait(timeout=5);peer_process,tokens[1]=daemon(1)
        assert msg('get',instance=aid)['definition']['id']==aid
        assert msg('list')['issues']
        reject(lambda:msg('get',instance=bid))
        # A failed connector directory cannot prevent the daemon from serving existing mail/work.
        shutil.rmtree(homes[1]/'connectors');(homes[1]/'connectors').write_text('unavailable registry')
        peer_process.kill();peer_process.wait(timeout=5);peer_process,tokens[1]=daemon(1)
        assert msg('list')['issues']
        assert api({'op':'resident_get','id':r1})['id']==r1
        assert api({'op':'list'})
        activate(r1)
        content=log.read_text();assert 'messaging.instance_created' in content and 'messaging.intake_accepted' in content and 'messaging.operation' in content
        assert 'fixture-a' not in content and 'fixture-b' not in content and 'SILENT native input' not in content
        print(json.dumps({'messaging_smoke':'passed','instances':2,'resident_bindings':3,'native_account_isolation':True,'explicit_bot_sharing':True,'subject_free_send':True,'reply_provenance':True,'private_narration':True,'uncertain_effect_not_replayed':True,'restart_continuity':True,'bad_instance_isolation':True,'discord_backend':'controlled_http'}))
    finally:
        server.shutdown();server.server_close()
        for p in processes:
            if p.poll() is None:p.terminate()
        for p in processes:
            try:p.wait(timeout=5)
            except subprocess.TimeoutExpired:p.kill();p.wait()
