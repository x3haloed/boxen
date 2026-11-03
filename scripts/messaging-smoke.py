#!/usr/bin/env python3
"""Real paired daemons, resident tools, isolated connector accounts and controlled Discord HTTP."""
import http.server
import email.parser
import email.policy
from smoke_helpers import fake_codex, windows_cpu
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
import urllib.parse

ROOT=pathlib.Path(__file__).resolve().parent.parent
BINARY=pathlib.Path(sys.argv[1] if len(sys.argv)>1 else ROOT/'target/release/boxen').resolve()
COMMAND=(['/bin/bash'] if BINARY.suffix=='.com' else [])+[str(BINARY)]
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]

class Discord(http.server.BaseHTTPRequestHandler):
    context_failure=False
    messages=[];sent=[];uploads=[];counter=100;rate_limit=False;unknown=False;reject_after=0;unknown_after=0;crash_after=0
    def log_message(self,*args):pass
    def reply(self,status,value):
        data=json.dumps(value).encode();self.send_response(status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def account(self):
        return {'Bot fixture-a':{'id':'1001','bot':True,'username':'First bot'},'Bot fixture-b':{'id':'1002','bot':True,'username':'Second bot'}}.get(self.headers.get('Authorization'))
    def do_GET(self):
        if self.path=='/attachment.bin':
            data=b'url fixture bytes\x00\xff';self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data);return
        account=self.account()
        if not account:self.reply(401,{'error':'bad credential'});return
        if self.path=='/users/@me':self.reply(200,account);return
        if self.path=='/channels/666' and Discord.context_failure:self.reply(503,{'error':'context unavailable'});return
        if self.path.startswith('/channels/') and self.path.count('/')==2:
            channel=self.path.rsplit('/',1)[1];self.reply(200,{'id':channel,'guild_id':None if channel=='555' else 'space-one','type':1 if channel=='555' else 3 if channel=='666' else 0,'recipients':[{'id':'3333','bot':False}]});return
        if self.path.startswith('/channels/999/messages/'):
            ident=self.path.rsplit('/',1)[1];value=next((m for m in Discord.messages if m['id']==ident),None)
            self.reply(200 if value else 404,value or {'error':'missing'});return
        assert self.path.startswith('/channels/999/messages?'),self.path
        query=urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
        before=int(query.get('before',[str(2**64)])[0]);after=int(query.get('after',['0'])[0]);limit=int(query.get('limit',['25'])[0])
        selected=[m for m in Discord.messages if after<int(m['id'])<before]
        self.reply(200,(selected if 'after' in query else list(reversed(selected)))[:limit])
    def do_PUT(self):
        assert self.account();assert self.path.startswith('/channels/999/messages/') and '/reactions/' in self.path
        self.send_response(204);self.send_header('Content-Length','0');self.end_headers()
    def do_POST(self):
        account=self.account();assert account
        raw=self.rfile.read(int(self.headers['Content-Length']))
        if self.headers.get('Content-Type','').startswith('multipart/'):
            mime=email.parser.BytesParser(policy=email.policy.default).parsebytes(('Content-Type: '+self.headers['Content-Type']+'\r\nMIME-Version: 1.0\r\n\r\n').encode()+raw)
            fields=list(mime.iter_parts());body=json.loads(next(p for p in fields if p.get_param('name',header='content-disposition')=='payload_json').get_payload(decode=True))
            Discord.uploads.extend({'filename':p.get_filename(),'bytes':p.get_payload(decode=True)} for p in fields if p.get_param('name','',header='content-disposition')!='payload_json')
        else:body=json.loads(raw)
        assert body['allowed_mentions']=={'parse':[]},body
        assert len(body['content'].encode('utf-16-le'))//2<=2000,body
        if Discord.reject_after:
            Discord.reject_after-=1
            if not Discord.reject_after:self.reply(400,{'error':'controlled part rejection'});return
        if Discord.rate_limit:Discord.rate_limit=False;self.reply(429,{'retry_after':0.01,'global':False});return
        value=add(body['content'],account);Discord.sent.append({'account':account['id'],'body':body,'id':value['id']})
        if Discord.crash_after:
            Discord.crash_after-=1
            if not Discord.crash_after:peer_process.kill();peer_process.wait(timeout=5)
        if Discord.unknown_after:
            Discord.unknown_after-=1
            if not Discord.unknown_after:Discord.unknown=True
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
        p=subprocess.Popen(COMMAND+['--data-dir',str(homes[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}','--codex',str(fake_codex())],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
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
        d={'id':secrets.token_hex(32),'name':name,'provider':'discord','account':account,'credential_file':str(credential),'capabilities':['send','receive','replies','native_metadata'],'bindings':[{'conversation':'999','resident':r,'accept_bots':True} for r in residents]}
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
        def local_connector(instance,action,**kwargs):
            body={'peer':keys[1],'connector_instance':instance,'request':{'op':'messaging','args':{'instance':instance,'action':action,**kwargs}}}
            r=urllib.request.Request(f'http://127.0.0.1:{uis[1]}/api/operation',headers={'Authorization':'Bearer '+tokens[1],'Content-Type':'application/json'},data=json.dumps(body).encode())
            return json.load(urllib.request.urlopen(r,timeout=20))
        assert local_connector(aid,'get')['owner']==keys[0]
        def scoped_request(body,i=1):
            r=urllib.request.Request(f'http://127.0.0.1:{uis[i]}/api/operation',headers={'Authorization':'Bearer '+tokens[i],'Content-Type':'application/json'},data=json.dumps(body).encode())
            return json.load(urllib.request.urlopen(r,timeout=20))
        reject(lambda:scoped_request({'peer':keys[1],'connector_instance':aid,'request':{'op':'messaging','args':{'action':'get','instance':bid}}}))
        reject(lambda:scoped_request({'peer':keys[1],'connector_instance':aid,'request':{'op':'resident_get','id':r1}}))
        reject(lambda:scoped_request({'peer':keys[1],'address':f'127.0.0.1:{peers[1]}','connector_instance':aid,'request':{'op':'messaging','args':{'action':'get','instance':aid}}},0))

        reject(lambda:local_connector(aid,'configure_gateway',expected_revision=1,enabled=True))
        # Ordinary local APIs cannot claim a foreign owner; only the bounded runner route resolves it.
        reject(lambda:msg('get',i=1,instance=aid))
        local_key=homes[1]/'runner-local.key';local_key.write_text('fixture-a');local_key.chmod(0o600)
        def local_runner_once():
            r=subprocess.run(COMMAND+['--data-dir',str(homes[1]),'discord-connector','--instance',aid,'--credential-file',str(local_key),'--url',f'http://127.0.0.1:{uis[1]}','--api-base',f'http://127.0.0.1:{server.server_port}','--once'],capture_output=True,text=True,timeout=30)
            assert r.returncode==0,(r.stdout,r.stderr)
        runner(a);runner(b)
        local_runner_once()
        native=add('SILENT native input')
        local_runner_once();runner(b)
        ia,ib,ic=inbox(r1),inbox(r2),inbox(r3)
        assert len(ia)==len(ib)==len(ic)==1
        assert ia[0]['id']!=ib[0]['id']!=ic[0]['id']
        assert ia[0]['origin']['account']=='1001' and ib[0]['origin']['account']=='1002'
        assert ia[0]['origin']['native_id']==ib[0]['origin']['native_id']==native['id']
        assert ia[0]['from']['instance']==aid and ib[0]['from']['instance']==bid
        assert ia[0]['origin']['author']['id']=='2222'
        own_a=add('SILENT own account A',{'id':'1001','bot':True})
        own_b=add('SILENT own account B',{'id':'1002','bot':True})
        human=add('SILENT human after bots')
        runner(a);runner(b)
        ids=lambda resident:{m['origin']['native_id'] for m in inbox(resident) if m.get('origin',{}).get('provider')=='discord'}
        assert own_a['id'] not in ids(r1) and own_b['id'] in ids(r1) and human['id'] in ids(r1), inbox(r1)
        for resident_id in [r2,r3]:
            assert own_b['id'] not in ids(resident_id) and own_a['id'] in ids(resident_id) and human['id'] in ids(resident_id)
        tail=add('SILENT self-only tail',{'id':'1001','bot':True});before=len(inbox(r1))
        skipped=runner(a)
        assert msg('cursor',instance=aid,conversation='999')['checkpoint']['cursor']==tail['id']
        assert any(json.loads(line).get('event')=='discord.self_echo_skipped' and json.loads(line)['fields']['count']==1 for line in skipped.stdout.splitlines())
        runner(a);assert len(inbox(r1))==before
        local=command(r1,{'definition':{'id':'boxen'}},body='SILENT generic Boxen mail',conversation='resident:'+r2,subject='Local connector proof');assert any(m['id']==local and m['from']['instance']=='boxen' for m in inbox(r2))
        custom=msg('create',definition={'id':secrets.token_hex(32),'tools':[{'name':'lookup','description':'Returns public fixture data.','kind':'query','permission':'read','parameters':{'type':'object'}},{'name':'send_files','description':'Returns addon-defined public data; this is not Discord upload.','kind':'query','permission':'read','parameters':{'type':'object'}}],'name':'Credential-free custom addon','provider':'custom','account':'public-room','capabilities':['receive','native_metadata'],'settings':{'endpoint':'example.invalid'},'bindings':[{'conversation':'a:b','resident':r3,'send':False},{'conversation':'a','resident':r3,'send':False},{'conversation':'a','resident':r1,'send':False}]})
        custom_id=custom['definition']['id']
        for conversation,native_id in [('a:b','c'),('a','b:c')]:
            msg('ingest',instance=custom_id,conversation=conversation,after=None,cursor='one',messages=[{'id':native_id,'author':{'id':'external-author'},'body':'SILENT opaque identifier collision proof'}])
        assert len([m for m in inbox(r3) if m.get('origin',{}).get('instance')==custom_id])==2
        service=msg('call',resident=r3,id=secrets.token_hex(32),to={'instance':custom_id,'conversation':'a'},operation='lookup',arguments={'key':'example'})
        claim=secrets.token_hex(32)
        assert msg('claim',instance=custom_id,id=service['id'],claim_id=claim)['claimed']
        result={'value':'credential-free native query'}
        done=msg('complete',instance=custom_id,id=service['id'],claim_id=claim,status='accepted',result=result)
        assert msg('status',id=service['id'])['delivery']['result']==result
        assert msg('complete',instance=custom_id,id=service['id'],claim_id=claim,status='accepted',result=result)==done
        reject(lambda:msg('complete',instance=custom_id,id=service['id'],claim_id=claim,status='accepted',result={'value':'changed'}))

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
        long_body=('Unicode 🦊 and text\n'*220)+'END'
        before=len(Discord.sent);long_id=command(r1,a,long_body,reply=ia[0]['id']);Discord.rate_limit=True;runner(a)
        sent=Discord.sent[before:];receipt=msg('status',id=long_id)['delivery']
        assert ''.join(p['body']['content'] for p in sent)==long_body and len(sent)>1
        assert receipt['status']=='accepted' and receipt['parts_total']==len(sent)
        assert [p['external_id'] for p in receipt['parts']]==[p['id'] for p in sent]
        assert sent[0]['body']['message_reference']['message_id']==native['id']
        assert all('message_reference' not in p['body'] for p in sent[1:])
        assert len({p['body']['nonce'] for p in sent})==len(sent)
        runner(a);assert len(Discord.sent)==before+len(sent)
        reject(lambda:msg('part',instance=aid,id=long_id,claim_id=receipt['claim_id'],index=0,total=len(sent),status='accepted',external_id='different'))
        for boundary_body in ['x'*2000, '🦊'*1100+'tail', 'm'*32000]:
            before=len(Discord.sent);boundary=command(r1,a,boundary_body);runner(a)
            assert ''.join(p['body']['content'] for p in Discord.sent[before:])==boundary_body
            assert msg('status',id=boundary)['delivery']['status']=='accepted'
        partial=command(r1,a,'p'*4500);before=len(Discord.sent);Discord.reject_after=2;runner(a)
        receipt=msg('status',id=partial)['delivery'];assert receipt['status']=='uncertain' and receipt['reason']=='partial_delivery'
        assert [p['status'] for p in receipt['parts']]==['accepted','rejected'] and len(Discord.sent)==before+1
        lost=command(r1,a,'q'*4500);before=len(Discord.sent);Discord.unknown_after=2;runner(a)
        receipt=msg('status',id=lost)['delivery'];assert [p['status'] for p in receipt['parts']]==['accepted','uncertain']
        assert len(Discord.sent)==before+2
        crashed=command(r1,a,'z'*4500);before=len(Discord.sent);Discord.crash_after=2;runner(a,1)
        peer_process,tokens[1]=daemon(1)
        receipt=msg('status',id=crashed)['delivery'];assert receipt['status']=='uncertain'
        assert [p['status'] for p in receipt['parts']]==['accepted','claimed'] and receipt['parts_total']==3
        runner(a);assert len(Discord.sent)==before+2,'partial multipart effects replayed after restart'
        resumed=command(r1,a,'Fresh send after interrupted multipart');runner(a)
        assert msg('status',id=resumed)['delivery']['status']=='accepted'
        # Policy grants and resident attention are separate durable predicates.
        pc=instance('Policy connector','1001',[r1],'fixture-a');pid=pc['definition']['id']
        def policy(grants):
            current=msg('get',instance=pid)
            return msg('configure_policy',instance=pid,expected_revision=current['revision'],policy={'grants':grants})
        def grant(r,kind='conversation',ident='999',actions=None,effect='allow',bots=False):
            scope={'kind':kind}
            if ident is not None:scope['id']=ident
            return {'resident':r,'scope':scope,'actions':actions or ['receive','read','send'],'effect':effect,'accept_bots':bots}
        def interest(r,action,scope):
            current=msg('attention',instance=pid,resident=r)
            return msg(action,instance=pid,resident=r,expected_revision=current['revision'],scope=scope)
        def intake(conversation,native):
            old=msg('cursor',instance=pid,conversation=conversation)['checkpoint']
            return msg('ingest',instance=pid,conversation=conversation,after=old['cursor'] if old else None,cursor=native['id'],messages=[native])
        def received(r):return [m for m in inbox(r) if (m.get('origin') or {}).get('instance')==pid]
        policy([grant(r1,'all',None),grant(r2)])
        intake('999',{'id':'10001','author':{'id':'person'},'body':'SILENT ordinary'})
        assert len(received(r1))==1 and not received(r2),'permission alone subscribed resident'
        interest(r2,'watch',{'kind':'all'})
        intake('888',{'id':'10002','author':{'id':'person'},'body':'SILENT outside grant'})
        assert not received(r2),'attention granted permission'
        interest(r1,'mute',{'kind':'conversation','id':'999'})
        intake('999',{'id':'10003','author':{'id':'person'},'body':'SILENT muted'})
        assert len(received(r1))==1 and len(received(r2))==1
        queued=command(r1,pc,'SILENT outbound while muted')
        policy([grant(r1,'all',None),grant(r1,'all',None,['send'],'deny'),grant(r2)])
        # Claim rejection is retained and does not block other new sends.
        before=len(Discord.sent);runner(pc)
        assert msg('status',id=queued)['delivery']['reason']=='policy_denied' and len(Discord.sent)==before
        next_send=command(r2,pc,'SILENT independent outbound');runner(pc)
        assert msg('status',id=next_send)['delivery']['status']=='accepted'
        latest=msg('get',instance=pid)
        reject(lambda:msg('configure_policy',instance=pid,expected_revision=1,policy={'grants':[]}))
        assert msg('get',instance=pid)==latest
        # Policy can be wider than Watch, while attention can still select only directed mail.
        policy([grant(r2,'all',None)])
        interest(r2,'unwatch',{'kind':'all'})
        intake('777',{'id':'10004','author':{'id':'person'},'body':'SILENT unselected'})
        before=len(received(r2))
        intake('777',{'id':'10005','author':{'id':'person'},'body':'SILENT directed','mention':True})
        assert len(received(r2))==before+1
        policy([grant(r2,'all',None),grant(r2,'user','person',['receive'],'deny')])
        intake('777',{'id':'10006','author':{'id':'person'},'body':'SILENT denied author','mention':True})
        assert len(received(r2))==before+1
        policy([grant(r2,'space','guild-one'),grant(r2,'parent','blocked',['receive'],'deny')])
        before=len(received(r2))
        intake('777',{'id':'10007','space':'guild-other','parent':'allowed','author':{'id':'person'},'body':'SILENT wrong guild','mention':True})
        intake('777',{'id':'10008','space':'guild-one','parent':'blocked','author':{'id':'person'},'body':'SILENT denied parent','mention':True})
        assert len(received(r2))==before
        intake('777',{'id':'10009','space':'guild-one','parent':'allowed','author':{'id':'person'},'body':'SILENT allowed guild','mention':True})
        assert len(received(r2))==before+1
        assert msg('native_presence',instance=pid)['status']=='invisible'
        policy(msg('get',instance=pid)['policy']['grants']+[grant(r2,'all',None,['presence'])])
        assert msg('native_presence',instance=pid)['status']=='idle','unpresented mail did not show queued presence'
        allowed='whitelisted-user'
        deny=grant(r2,'direct',None,['receive'],'deny');deny['except_users']=[allowed]
        policy([grant(r2,'all',None),deny])
        before=len(received(r2))
        intake('555',{'id':'10010','direct':True,'author':{'id':'other'},'body':'SILENT unlisted DM','mention':True})
        intake('555',{'id':'10011','direct':True,'author':{'id':allowed},'body':'SILENT listed DM'})
        intake('777',{'id':'10012','space':'guild-one','author':{'id':'other'},'body':'SILENT guild mention','mention':True})
        assert len(received(r2))==before+2,'DM restriction either leaked or blocked allowed/guild correspondence'
        saved_policy=msg('get',instance=pid)
        peer_process.kill();peer_process.wait(timeout=5);peer_process,tokens[1]=daemon(1)
        assert msg('get',instance=pid)==saved_policy
        assert msg('create',definition=pc['definition'])==saved_policy,'creation retry reset policy or attention'
        # Native context and reactions travel through retained generic service requests.
        policy([grant(r1,'user','3333',['react'])])
        group=msg('call',resident=r1,id=secrets.token_hex(32),to={'instance':pid,'conversation':'666'},operation='react',arguments={'message_id':'12345','emoji':'🦊'})
        runner(pc)
        assert msg('status',id=group['id'])['delivery']['reason']=='policy_denied','one DM correspondent granted a group-room action'
        policy([grant(r1,'conversation','999',['read','receive','send','react'])])
        def call_service(operation,arguments):
            return msg('call',resident=r1,id=secrets.token_hex(32),to={'instance':pid,'conversation':'999'},operation=operation,arguments=arguments)
        query=call_service('read_context',{'limit':3});runner(pc)
        answer=msg('status',id=query['id'])
        assert answer['service']['operation']=='read_context' and answer['delivery']['status']=='accepted'
        window=answer['delivery']['result'];assert len(window['messages'])==3
        assert [int(m['id']) for m in window['messages']]==sorted(int(m['id']) for m in window['messages'])
        center=call_service('read_context',{'message_id':native['id'],'before':0,'after':2});runner(pc)
        assert any(m['id']==native['id'] for m in msg('status',id=center['id'])['delivery']['result']['messages'])
        reaction=call_service('react',{'message_id':native['id'],'emoji':'🦊'});runner(pc)
        assert msg('status',id=reaction['id'])['delivery']['status']=='accepted'
        # Unavailable metadata cannot permanently occupy the bounded dispatch window.
        policy([grant(r1,'all',None,['read','receive','send','react'])])
        blocked=[msg('call',resident=r1,id=secrets.token_hex(32),to={'instance':pid,'conversation':'666'},operation='react',arguments={'message_id':'12345','emoji':'🦊'}) for _ in range(20)]
        healthy=call_service('react',{'message_id':native['id'],'emoji':'🦊'})
        Discord.context_failure=True
        try:runner(pc);runner(pc)
        finally:Discord.context_failure=False
        assert all(msg('status',id=m['id'])['delivery']['status']=='rejected' for m in blocked)
        assert msg('status',id=healthy['id'])['delivery']['status']=='accepted','unavailable conversations blocked healthy service work'
        policy([grant(r1,'conversation','999',['read','receive','send'])])
        reject(lambda:call_service('react',{'message_id':native['id'],'emoji':'🦊'}))
        old_receipt=msg('status',id=query['id'])
        peer_process.kill();peer_process.wait(timeout=5);peer_process,tokens[1]=daemon(1)
        assert msg('status',id=query['id'])==old_receipt
        policy([grant(r1,'conversation','999',['read','receive','send','files'])])
        def agent_service(call,before_dispatch=None):
            api({'op':'mailbox_send','client_id':keys[0],'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':r1},'subject':'Native service proof','body':'MESSAGING:'+json.dumps(call)}})
            errors=[]
            def execute():
                try:activate(r1)
                except Exception as error:errors.append(error)
            worker=threading.Thread(target=execute);worker.start();deadline=time.monotonic()+8
            destination=call['to']['instance']
            while not any(m['id']==call['id'] for m in msg('outbox',instance=destination)['messages']):
                assert time.monotonic()<deadline;time.sleep(.04)
            if before_dispatch:before_dispatch()
            if destination==pid:runner(pc)
            else:
                claim=secrets.token_hex(32);assert msg('claim',instance=destination,id=call['id'],claim_id=claim)['claimed']
                msg('complete',instance=destination,id=call['id'],claim_id=claim,status='accepted',result={'custom':True})
            worker.join(10);assert not worker.is_alive() and not errors,errors
            return msg('status',id=call['id'])
        custom_call={'action':'call','id':secrets.token_hex(32),'to':{'instance':custom_id,'conversation':'a'},'operation':'send_files','arguments':{'key':'addon semantics'}}
        assert agent_service(custom_call)['delivery']['result']=={'custom':True},'Discord upload behavior leaked into a custom provider'
        artifact=directory/'opaque.data';payload=b'opaque attachment bytes\x00\xff\n';artifact.write_bytes(payload)
        upload={'action':'call','id':secrets.token_hex(32),'to':{'instance':pid,'conversation':'999'},'operation':'send_files','arguments':{'paths':[str(artifact)],'body':'u'*2500}}
        before=len(Discord.sent);sent=agent_service(upload,artifact.unlink)
        assert sent['delivery']['status']=='accepted' and Discord.uploads[-1]['bytes']==payload
        assert ''.join(m['body']['content'] for m in Discord.sent[before:])=='u'*2500
        assert len(sent['delivery']['parts'])==2
        # Same-ID retry works after the original file disappears, with no new upload.
        count=len(Discord.uploads)
        api({'op':'mailbox_send','client_id':keys[0],'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':r1},'subject':'Retry upload','body':'MESSAGING:'+json.dumps(upload)}})
        activate(r1);runner(pc);assert len(Discord.uploads)==count
        reject(lambda:msg('attachment',instance=aid,id=upload['id'],index=0))
        url_upload={'action':'call','id':secrets.token_hex(32),'to':{'instance':pid,'conversation':'999'},'operation':'send_files','arguments':{'paths':[f'http://127.0.0.1:{server.server_port}/attachment.bin']}}
        assert agent_service(url_upload)['delivery']['status']=='accepted'
        assert Discord.uploads[-1]['bytes']==b'url fixture bytes\x00\xff'
        # Real WebSocket handshake/heartbeats/resume against a controlled Gateway.
        sys.path.insert(0,str(ROOT/'tests'))
        from discord_gateway_fixture import GatewayFixture
        gateway_fixture=GatewayFixture();gateway_process=None
        def eventually(predicate,seconds=12):
            deadline=time.monotonic()+seconds
            while time.monotonic()<deadline:
                value=predicate()
                if value:return value
                time.sleep(.05)
            raise AssertionError('Gateway state did not converge')
        gateway_log=None
        try:
            gr=resident('Gateway resident')
            api({'op':'resident_update','id':gr,'patch':{'expected_revision':1,'auto_activate':True}})
            gid=secrets.token_hex(32)
            gi=msg('create',definition={'id':gid,'name':'Gateway connector','provider':'discord','account':'1001','credential_file':a['definition']['credential_file'],'settings':{'gateway_url':gateway_fixture.url},'capabilities':['send','receive','replies','native_metadata'],'bindings':[{'conversation':'999','resident':gr}]})
            msg('configure_gateway',instance=gid,expected_revision=gi['revision'],enabled=True)
            gateway_log=(directory/'gateway.log').open('w')
            def start_gateway():
                return subprocess.Popen(COMMAND+['--data-dir',str(homes[0]),'discord-connector','--instance',gid,'--url',f'http://127.0.0.1:{uis[0]}','--peer',keys[1],'--address',f'127.0.0.1:{peers[1]}','--api-base',f'http://127.0.0.1:{server.server_port}'],stdout=gateway_log,stderr=subprocess.STDOUT)
            gateway_process=start_gateway();processes.append(gateway_process)
            eventually(lambda:msg('gateway_state',instance=gid)['session']['sequence']>=2)
            def gm(ident,channel='999',mention=False,author='person'):
                return {'id':ident,'channel_id':channel,'guild_id':None if channel=='555' else 'space-one','author':{'id':author,'bot':author=='1001'},'content':'SILENT Gateway input '+ident,'mentions':[{'id':'1001'}] if mention else [],'timestamp':'2026-11-01T00:00:00Z','attachments':[]}
            def gmessages():return [m for m in inbox(gr) if (m.get('origin') or {}).get('instance')==gid]
            first_seq=gateway_fixture.push('MESSAGE_CREATE',gm('50000'))
            eventually(lambda:len(gmessages())==1)
            gateway_fixture.push('MESSAGE_CREATE',gm('50001',author='1001'))
            denied_seq=gateway_fixture.push('MESSAGE_CREATE',gm('50002','777',True))
            eventually(lambda:msg('gateway_state',instance=gid)['session']['sequence']>=denied_seq)
            assert len(gmessages())==1,'Gateway bypassed restrictive defaults or self exclusion'
            current=msg('get',instance=gid)
            msg('configure_policy',instance=gid,expected_revision=current['revision'],policy={'grants':[grant(gr,'all',None,['receive','read','send','presence'])]})
            directed=gateway_fixture.push('MESSAGE_CREATE',gm('50003','777',True));gateway_fixture.push('MESSAGE_CREATE',gm('50004','555'))
            ordinary=gateway_fixture.push('MESSAGE_CREATE',gm('50005','777'))
            eventually(lambda:msg('gateway_state',instance=gid)['session']['sequence']>=ordinary)
            assert {m['origin']['native_id'] for m in gmessages()}=={'50000','50003','50004'}
            eventually(lambda:any(p['status']=='online' for p in gateway_fixture.presences),15)
            # Native replay resumes at durable acceptance and never duplicates intake.
            gateway_process.terminate();gateway_process.wait(timeout=5)
            replay=gateway_fixture.push('MESSAGE_CREATE',gm('50006'))
            gateway_process=start_gateway();processes.append(gateway_process)
            eventually(lambda:msg('gateway_state',instance=gid)['session']['sequence']>=replay,15)
            assert any(a['op']==6 for a in gateway_fixture.auth)
            assert len([m for m in gmessages() if m['origin']['native_id']=='50006'])==1
            # Passive own-message reactions are observations, never incoming mail/wakes.
            eventually(lambda:all(r['status'] in ('completed','interrupted','failed','cancelled') for r in api({'op':'list'})))
            runs_before=len(api({'op':'list'}));mail_before=len(gmessages())
            reaction=gateway_fixture.push('MESSAGE_REACTION_ADD',{'channel_id':'999','guild_id':'space-one','message_id':'60000','message_author_id':'1001','user_id':'person','emoji':{'name':'🦊','id':None}})
            eventually(lambda:msg('gateway_state',instance=gid)['session']['sequence']>=reaction)
            sources=api({'op':'events','args':{'action':'list'}})['sources'];source=next(s for s in sources if s['name']=='Discord reactions')
            assert source['retained']==1 and source['audience']==gr
            reject(lambda:api({'op':'events','args':{'action':'subscribe','spec':{'id':secrets.token_hex(32),'source':source['id'],'target':{'type':'resident','id':r1},'interval_ms':1000,'lifetime':'persistent'}}}))
            def cpu():
                if os.name == 'nt':return windows_cpu(gateway_process)
                value=subprocess.check_output(['ps','-p',str(gateway_process.pid),'-o','time='],text=True).strip();return sum(float(v)*(60**i) for i,v in enumerate(reversed(value.split(':'))))
            before_cpu=cpu();time.sleep(2);assert cpu()-before_cpu<.15,'Gateway idle transport spins'
            assert len(api({'op':'list'}))==runs_before and len(gmessages())==mail_before
            # Invalid resume is legible, then a fresh session accepts new work.
            gateway_fixture.invalidate=True;gateway_fixture.disconnect()
            eventually(lambda:msg('gateway_state',instance=gid)['session']['gaps']>=1,15)
            eventually(lambda:msg('gateway_state',instance=gid)['session']['session_id']=='fixture-session-two',25)
            fresh=gateway_fixture.push('MESSAGE_CREATE',gm('50007'))
            eventually(lambda:any(m['origin']['native_id']=='50007' for m in gmessages()))
            assert 'fixture-a' not in (directory/'gateway.log').read_text()
            print('PASS native Gateway directed/ordinary attention, restrictive policy, self exclusion, presence, durable resume, passive private reactions, invalid-session recovery and idle CPU')
        finally:
            if gateway_process and gateway_process.poll() is None:gateway_process.terminate();gateway_process.wait(timeout=5)
            if gateway_log:gateway_log.close()
            gateway_fixture.close()
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
        print(json.dumps({'messaging_smoke':'passed','instances':2,'resident_bindings':3,'native_account_isolation':True,'explicit_bot_sharing':True,'self_echo_excluded':True,'self_only_cursor_advances':True,'other_bot_intake':True,'multipart_exact_unicode':True,'multipart_partial_and_crash_recovery':True,'subject_free_send':True,'reply_provenance':True,'private_narration':True,'uncertain_effect_not_replayed':True,'restart_continuity':True,'bad_instance_isolation':True,'discord_backend':'controlled_http'}))
    finally:
        server.shutdown();server.server_close()
        for p in processes:
            if p.poll() is None:p.terminate()
        for p in processes:
            try:p.wait(timeout=5)
            except subprocess.TimeoutExpired:p.kill();p.wait()
