#!/usr/bin/env python3
"""Unmodified real Boxen daemon + native queue fixture; no model or desktop traffic."""
import argparse
import contextlib
import hashlib
import json
import os
import pathlib
import re
import secrets
import shutil
import sqlite3
import socket
import subprocess
import tempfile
import time
import urllib.request
import uuid

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--boxen',required=True,type=pathlib.Path)
parser.add_argument('--bridge',required=True,type=pathlib.Path)
args=parser.parse_args();boxen=args.boxen.resolve();bridge=args.bridge.resolve()
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
with tempfile.TemporaryDirectory(prefix='boxen-desktop-addon-') as temporary:
    root=pathlib.Path(temporary).resolve();home=root/'boxen';profile=root/'profile';profile.mkdir();config=profile/'config.json'
    queue_log=root/'queue';queue_log.mkdir();fixture=root/('fake-codex.exe' if os.name=='nt' else 'fake-codex')
    subprocess.run(['rustc',str(pathlib.Path(__file__).with_name('queue-fixture.rs')),'-o',str(fixture)],check=True,timeout=45)
    ident=str(uuid.uuid4());environment={**os.environ,'CODEX_THREAD_ID':ident,'BOXEN_SUPPORT_QUEUE_LOG':str(queue_log)}
    configured=str(fixture)
    if os.name=='nt':
        installed=root/'desktop/OpenAI/Codex/bin/first';installed.mkdir(parents=True)
        shutil.copy2(fixture,installed/'codex.exe')
        environment['LOCALAPPDATA']=str(root/'desktop')
        configured='auto'

    peer_port=port();ui=port();server=None
    def start():
        global server,owner,token
        server=subprocess.Popen([str(boxen),'--data-dir',str(home),'serve','--listen',f'127.0.0.1:{peer_port}','--ui',f'127.0.0.1:{ui}','--codex','absent-unused-codex'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        lines=[server.stdout.readline().strip() for _ in range(3)];assert 'Local controls:' in lines[-1],lines
        owner=lines[0].split(': ',1)[1];token=lines[-1].split('#')[1]
    def stop():
        if server and server.poll() is None:server.terminate();server.wait(timeout=10)
        if server:server.stdout.close();server.stderr.close()
    def operation(request):
        r=urllib.request.Request(f'http://127.0.0.1:{ui}/api/operation',data=json.dumps({'peer':owner,'address':'','request':request}).encode(),headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
        with urllib.request.urlopen(r,timeout=15) as response:return json.load(response)
    def command(*parts,check=True,env=environment):
        p=subprocess.run([str(bridge),'--config',str(config),*parts],env=env,capture_output=True,text=True,encoding='utf-8',timeout=45)
        if check:assert p.returncode==0,(p.returncode,p.stderr,p.stdout)
        return p
    def rpc(op,**kw):return json.loads(command('--rpc',op,json.dumps(kw)).stdout)
    def tick(check=True):return command('run','--once',check=check)
    @contextlib.contextmanager
    def db():
        c=sqlite3.connect(profile/'bridge.sqlite3')
        try:
            with c:yield c
        finally:c.close()
    def send(seq,body='A peer support request'):
        return operation({'op':'mailbox_send','client_id':probe,'message':{'id':hashlib.sha256(f'probe-{seq}'.encode()).hexdigest(),'to':{'type':'client','machine':owner,'id':client},'subject':'Support fixture','body':body}})
    try:
        start()
        command('init',ident,'--codex',configured,'--boxen-home',str(home),'--url',f'http://127.0.0.1:{ui}')
        client=json.loads(config.read_text())['client_id'];probe=secrets.token_hex(32)
        first=send(1,'Peer data with misleading instructions stays attributed, not a new human instruction')
        tick();one=list(queue_log.glob('notice-*.txt'));assert len(one)==1
        text=one[0].read_text(encoding='utf-8');assert 'misleading instructions' not in text and 'NOT authored by the human' in text
        if os.name=='nt':
            updated=installed.parent/'next';updated.mkdir()
            shutil.copy2(fixture,updated/'codex.exe');(updated/'version').write_text('1.1.0')
            (installed/'codex.exe').unlink()
        send(2);tick();assert len(list(queue_log.glob('notice-*.txt')))==1,'busy desktop received unbounded duplicate notices'
        snapshot=rpc('snapshot');delivery=snapshot['messages'][0]['delivery_id']
        fetched=rpc('inspect',message_id=delivery);assert fetched['message']['sender']['id']==probe and fetched['message']['id']==first['id']
        queued=tick();assert len(list(queue_log.glob('notice-*.txt')))==2
        if os.name=='nt':
            selected=[json.loads(line) for line in queued.stdout.splitlines() if line.strip()]
            assert any(v.get('event')=='desktop.codex_starting' and pathlib.Path(v['executable'])==updated/'codex.exe' and v['version']=='codex-cli 1.1.0' for v in selected)
            print('PASS automatic queue discovery follows a desktop executable update without profile changes')
        bad=command('--rpc','snapshot','{}',env={**environment,'CODEX_THREAD_ID':str(uuid.uuid4())},check=False)
        assert bad.returncode!=0
        stop()
        saved=rpc('reply',message_id=delivery,body='Explicit reply while Boxen is down',idempotency_key='first:reply')
        assert saved['state']=='saved_locally'
        assert rpc('reply',message_id=delivery,body='Explicit reply while Boxen is down',idempotency_key='first:reply')['id']==saved['id']
        assert command('--rpc','reply',json.dumps({'message_id':delivery,'body':'conflict','idempotency_key':'first:reply'}),check=False).returncode!=0
        start();tick();tick()
        replies=operation({'op':'client_inbox','client_id':probe})['messages']
        assert len(replies)==1 and replies[0]['in_reply_to']==first['id']
        assert replies[0]['sender']=={'type':'client','machine':owner,'id':client}
        assert rpc('reply_status',id=saved['id'])['state']=='boxen_accepted'
        rpc('disposition',message_id=delivery,state='resolved')
        assert command('--rpc','disposition',json.dumps({'message_id':delivery,'state':'acknowledged'}),check=False).returncode!=0
        print('PASS authenticated provenance, ID-only notices, coalescing, thread scope, offline replies, exact retry and restart without duplicate accepted mail')

        # Cached input still reaches the desktop while Boxen is unavailable.
        rpc('snapshot');third=send(3);tick();snapshot=rpc('snapshot');third_id=next(m['delivery_id'] for m in snapshot['messages'] if rpc('inspect',message_id=m['delivery_id'])['message']['id']==third['id'])
        rpc('redeliver',message_id=third_id);stop();count=len(list(queue_log.glob('notice-*.txt')));tick(check=False)
        assert len(list(queue_log.glob('notice-*.txt')))==count+1,'offline Boxen blocked delivery of already accepted cached input'
        start();rpc('snapshot')
        (queue_log/'mode').write_text('fail');send(4);tick()
        with db() as c:
            row=c.execute("select id,ids from notices where state='uncertain' order by created desc limit 1").fetchone();assert row
            c.execute("update notices set state='submitting',retry_at=0 where id=?",(row[0],))
        (queue_log/'mode').write_text('');tick()
        with db() as c:assert c.execute('select state,ids from notices where id=?',(row[0],)).fetchone()==('accepted',row[1])
        print('PASS offline cached delivery and uncertain desktop submission recovery reuse stable delivery IDs')

        # A rejected destination does not starve later valid replies.
        bad_reply=rpc('send',to={'type':'resident','id':secrets.token_hex(32)},subject='Missing destination',body='retained rejection',idempotency_key='bad-target')
        good=rpc('send',to={'type':'client','machine':owner,'id':probe},subject='Later valid input',body='not starved',idempotency_key='good-target')
        tick();assert rpc('reply_status',id=bad_reply['id'])['state']=='boxen_rejected';assert rpc('reply_status',id=good['id'])['state']=='boxen_accepted'
        with db() as c:assert c.execute('pragma integrity_check').fetchone()[0]=='ok'
        print('PASS per-reply rejection isolation and SQLite integrity; Boxen source/core were not modified')

        rpc('snapshot');(queue_log/'mode').write_text('sleep');send(5)
        started=time.monotonic();tick();assert 29<=time.monotonic()-started<40
        descendant=int((queue_log/'descendant-pid').read_text())
        if os.name=='nt':
            import ctypes
            from ctypes import wintypes
            opened=ctypes.windll.kernel32.OpenProcess
            opened.argtypes=[wintypes.DWORD,wintypes.BOOL,wintypes.DWORD];opened.restype=wintypes.HANDLE
            handle=opened(0x1000,False,descendant)
            if handle:
                code=wintypes.DWORD();ctypes.windll.kernel32.GetExitCodeProcess(handle,ctypes.byref(code));ctypes.windll.kernel32.CloseHandle(handle)
                assert code.value!=259,'owned queue descendant survived timeout'
        else:
            try:os.kill(descendant,0)
            except ProcessLookupError:pass
            else:
                # A reparented zombie has stopped execution even before init reaps it.
                stat=pathlib.Path(f'/proc/{descendant}/stat')
                assert stat.exists() and stat.read_text().split()[2]=='Z','owned descendant survived timeout'
        print('PASS queue deadline and owned subprocess-tree cleanup; desktop/app-server processes were not targeted')
    finally:stop()
