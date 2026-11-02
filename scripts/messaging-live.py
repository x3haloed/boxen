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
        msg('create',definition=definition)
        connector_log=(evidence/'connector.jsonl').open('w')
        runner=subprocess.Popen(command+['--data-dir',str(home),'discord-connector','--instance',ident,'--url',f'http://127.0.0.1:{ui}'],stdout=connector_log,stderr=subprocess.STDOUT,text=True,start_new_session=True);processes.append(runner)
        until(lambda:msg('cursor',instance=ident,conversation=args.channel)['checkpoint'],60)
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
