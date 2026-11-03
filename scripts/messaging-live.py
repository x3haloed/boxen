#!/usr/bin/env python3
"""Opt-in real Discord + Codex proof in an isolated temporary Boxen home.

Posts a greeting to the explicitly supplied channel, then waits for a human reply.
Credentials stay in their original private file; production residents are untouched.
"""
import argparse
import datetime
import hashlib
import json
import os
import pathlib
import secrets
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request

ROOT=pathlib.Path(__file__).resolve().parent.parent
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=pathlib.Path,default=ROOT/'target/release/boxen')
p.add_argument('--key-file',type=pathlib.Path,required=True)
p.add_argument('--account',required=True)
p.add_argument('--channel',required=True)
p.add_argument('--codex',default='codex')
p.add_argument('--model',default='')
p.add_argument('--gateway',action='store_true',help='Use native Gateway intake and test resume')
p.add_argument('--service-only',action='store_true',help='Check native history, reaction and retained upload without waiting for a human')
p.add_argument('--wait-seconds',type=int,default=1200)
args=p.parse_args()
binary=args.binary.resolve();binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest();command=(['/bin/bash'] if binary.suffix=='.com' else [])+[str(binary)]
def port():
    with socket.socket() as sock:sock.bind(('127.0.0.1',0));return sock.getsockname()[1]
stamp=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
evidence=ROOT/'work'/('messaging-live-'+stamp);evidence.mkdir(parents=True,exist_ok=True);evidence.chmod(0o700)
with tempfile.TemporaryDirectory(prefix='boxen-connector-live-') as temp:
    home=pathlib.Path(temp)/'home';peer,ui=port(),port();processes=[]
    cli=lambda *parts:subprocess.check_output(command+['--data-dir',str(home),*parts],text=True,timeout=15).strip()
    key=cli('identity');marker='boxen-'+secrets.token_hex(4)
    log=(evidence/'daemon.stderr').open('w')
    daemon=subprocess.Popen(command+['--data-dir',str(home),'serve','--listen',f'127.0.0.1:{peer}','--ui',f'127.0.0.1:{ui}','--codex',args.codex],stdout=subprocess.PIPE,stderr=log,text=True,start_new_session=True);processes.append(daemon)
    lines=[daemon.stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1]
    token=lines[-1].strip().split('#')[1]
    def api(request):
        r=urllib.request.Request(f'http://127.0.0.1:{ui}/api/operation',headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'},data=json.dumps({'peer':key,'address':'','request':request}).encode())
        return json.load(urllib.request.urlopen(r,timeout=30))
    def msg(action,**kw):return api({'op':'messaging','args':{'action':action,**kw}})
    def until(test,seconds=180):
        deadline=time.monotonic()+seconds
        while time.monotonic()<deadline:
            value=test()
            if value:return value
            time.sleep(.2)
        raise AssertionError('live proof did not reach the required state')
    try:
        resident=api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'backend':'codex','name':'Connector test (temporary)','cwd':temp,'model':args.model,'auto_activate':True,'identity':'This is a temporary resident for a human-requested messaging integration test. Assistant narration is private. External correspondence uses the declared messaging tools and connector bindings.'}})
        ident=secrets.token_hex(32)
        definition={'id':ident,'name':'Discord test bot','provider':'discord','account':args.account,'credential_file':str(args.key_file.resolve()),'capabilities':['send','receive','replies','native_metadata'],'bindings':[{'conversation':args.channel,'resident':resident['id']}]}
        created=msg('create',definition=definition)
        if args.service_only:
            created=msg('configure_policy',instance=ident,expected_revision=created['revision'],policy={'grants':[{'resident':resident['id'],'scope':{'kind':'conversation','id':args.channel},'actions':['read','receive','send','react','files']}]})
        if args.gateway:msg('configure_gateway',instance=ident,expected_revision=created['revision'],enabled=True)
        connector_log=(evidence/'connector.jsonl').open('w')
        runner=subprocess.Popen(command+['--data-dir',str(home),'discord-connector','--instance',ident,'--url',f'http://127.0.0.1:{ui}'],stdout=connector_log,stderr=subprocess.STDOUT,text=True,start_new_session=True);processes.append(runner)
        if args.gateway:until(lambda:msg('gateway_state',instance=ident)['session']['session_id'],60)
        else:until(lambda:msg('cursor',instance=ident,conversation=args.channel)['checkpoint'],60)
        greeting=f'Boxen connector test {marker}: a temporary resident is connected and ready for a reply.'
        directions=f'This is a human-requested messaging test. Its expected external result is exactly one greeting with body {json.dumps(greeting)} in connector instance {ident}, conversation {args.channel}. The greeting is explicit outgoing communication; final narration is private.'
        run=api({'op':'resident_activate','id':resident['id'],'activation_id':secrets.token_hex(32),'directions':directions})
        def greeting_sent():
            # Outbox exposes queued work only. Accepted sent messages are retained in the journal.
            ledger=home/'mailboxes/events.jsonl'
            events=[json.loads(line) for line in ledger.read_text().splitlines()]
            messages=[e['message'] for e in events if e['kind']=='message' and e['message'].get('body')==greeting]
            if not messages:return None
            current=msg('status',id=messages[0]['id']);return current if current['delivery']['status']=='accepted' else None
        first=until(greeting_sent)
        print(json.dumps({'live_ready':True,'channel':args.channel,'bot_account':args.account,'temporary_resident':resident['id'],'marker':marker,'greeting_external_id':first['delivery']['external_id'],'evidence':str(evidence)}),flush=True)
        (ROOT/'work/messaging-live-current.json').write_text(json.dumps({'channel':args.channel,'marker':marker,'resident':resident['id'],'instance':ident,'home':str(home),'ui':ui,'peer':peer,'pid':daemon.pid,'evidence':str(evidence)}))
        if args.service_only:
            def operation(name,arguments):
                receipt=msg('call',resident=resident['id'],id=secrets.token_hex(32),to={'instance':ident,'conversation':args.channel},operation=name,arguments=arguments)
                return until(lambda:(v if (v:=msg('status',id=receipt['id']))['delivery']['status'] in ('accepted','rejected') or v['delivery'].get('reason') else None))
            history=operation('read_context',{'message_id':first['delivery']['external_id'],'before':1,'after':1})
            assert history['delivery']['status']=='accepted',history
            assert any(m['id']==first['delivery']['external_id'] for m in history['delivery']['result']['messages'])
            reaction=operation('react',{'message_id':first['delivery']['external_id'],'emoji':'🦊'})
            assert reaction['delivery']['status']=='accepted',reaction
            artifact=pathlib.Path(temp)/'opaque.bin';artifact.write_bytes(b'Boxen explicit native upload test\x00\xff')
            upload_body='Boxen retained attachment test '+marker
            directions=f'This human-requested integration test has one expected external effect: a send_files call via boxen_messages to instance {ident}, conversation {args.channel}, with paths [{json.dumps(str(artifact))}] and body {json.dumps(upload_body)}. This call retains the file bytes before the connector sends them. Final narration is private.'
            api({'op':'resident_activate','id':resident['id'],'activation_id':secrets.token_hex(32),'directions':directions})
            def uploaded():
                events=[json.loads(line) for line in (home/'mailboxes/events.jsonl').read_text().splitlines()]
                for e in events:
                    m=e.get('message',{});service=m.get('service') or {}
                    if service.get('operation')=='send_files' and service['arguments'].get('body')==upload_body:
                        v=msg('status',id=m['id'])
                        if v['delivery']['status']=='accepted':return v
            upload=until(uploaded)
            native_id=upload['delivery']['external_id']
            checked=operation('read_context',{'message_id':native_id,'before':0,'after':0})
            assert checked['delivery']['status']=='accepted'
            attachments=checked['delivery']['result']['messages'][0]['attachments']
            assert attachments and attachments[0]['size']==artifact.stat().st_size,attachments
            resumed=False
            if args.gateway:
                saved=msg('gateway_state',instance=ident)['session']
                runner.terminate();runner.wait(timeout=10)
                previous_lines=len((evidence/'connector.jsonl').read_text().splitlines())
                runner=subprocess.Popen(command+['--data-dir',str(home),'discord-connector','--instance',ident,'--url',f'http://127.0.0.1:{ui}'],stdout=connector_log,stderr=subprocess.STDOUT,text=True,start_new_session=True);processes.append(runner)
                until(lambda:any(json.loads(line).get('event')=='discord.gateway_health' and json.loads(line).get('fields',{}).get('connected') for line in (evidence/'connector.jsonl').read_text().splitlines()[previous_lines:]),60)
                time.sleep(5)
                after=msg('gateway_state',instance=ident)['session'];assert after['session_id']==saved['session_id'] and after['gaps']==saved['gaps'],after
                resumed=True
            result={'ok':True,'binary_sha256':binary_sha256,'connector':'real Discord Gateway/REST' if args.gateway else 'real Discord REST','native_history':True,'reaction':True,'retained_upload':True,'resume_same_session':resumed,'incoming_human_tested':False}
            (evidence/'result.json').write_text(json.dumps(result,indent=2));print(json.dumps(result),flush=True)
            raise SystemExit(0)
        # A human message mentioning the bot works with Discord's mention-based content access too.
        def delivered_reply():
            events=[json.loads(line) for line in (home/'mailboxes/events.jsonl').read_text().splitlines()]
            incoming=[e['message'] for e in events if e['kind']=='message' and e['message'].get('origin') and marker in e['message']['body']]
            if not incoming:return None
            incoming_ids={m['id'] for m in incoming}
            for e in events:
                if e['kind']!='message':continue
                m=e['message']
                if m.get('in_reply_to') in incoming_ids and m['sender']=={'type':'resident','id':resident['id']}:
                    status=msg('status',id=m['id'])
                    if status['delivery']['status']=='accepted':return {'incoming':incoming,'outgoing':status}
            return None
        proof=until(delivered_reply,args.wait_seconds)
        until(lambda:all(r['status'] in ('completed','failed','interrupted','cancelled') for r in api({'op':'list'})),120)
        runs=api({'op':'list'});assert all(r['status']=='completed' for r in runs),runs
        result={'ok':True,'binary_sha256':binary_sha256,'backend':'real Codex app-server','connector':'real Discord HTTP','channel':args.channel,'account':args.account,'temporary_resident':resident['id'],'runs':len(runs),'greeting_external_id':first['delivery']['external_id'],'reply_external_id':proof['outgoing']['delivery']['external_id'],'native_author_preserved':all(m['origin']['author']['id']!=args.account for m in proof['incoming']),'in_reply_to':proof['outgoing']['in_reply_to'],'private_narration':True}
        (evidence/'result.json').write_text(json.dumps(result,indent=2));print(json.dumps(result),flush=True)
    finally:
        for process in reversed(processes):
            if process.poll() is None:os.killpg(process.pid,signal.SIGTERM)
        for process in processes:
            try:process.wait(timeout=10)
            except subprocess.TimeoutExpired:os.killpg(process.pid,signal.SIGKILL);process.wait()
        for name in ['mailboxes','runs','residents','connectors']:
            if (home/name).exists():shutil.copytree(home/name,evidence/name,dirs_exist_ok=True)
        log.close()
