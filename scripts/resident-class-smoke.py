#!/usr/bin/env python3
"""Versioned definitions, independent real resident activations and manual propagation."""
import concurrent.futures
import json
import os
import pathlib
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT=pathlib.Path(__file__).resolve().parent.parent
BINARY=pathlib.Path(sys.argv[1] if len(sys.argv)>1 else ROOT/'target/release/boxen').resolve()
COMMAND=(['/bin/bash'] if BINARY.suffix=='.com' else [])+[str(BINARY)]
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
with tempfile.TemporaryDirectory(prefix='boxen-resident-class-') as temp:
    root=pathlib.Path(temp);homes=[root/'a',root/'b'];processes=[];peers=[port(),port()];uis=[port(),port()]
    fixture=ROOT/'tests/fake-codex.py'
    if os.name=='nt':
        wrapper=root/'fake-codex.cmd'
        wrapper.write_text('@echo off\n"'+sys.executable+'" "'+str(fixture)+'" %*\n')
        fixture=wrapper
    else:fixture.chmod(0o755)
    def cli(i,*parts):return subprocess.check_output(COMMAND+['--data-dir',str(homes[i]),*parts],text=True,timeout=15).strip()
    keys=[cli(i,'identity') for i in range(2)]
    for i in range(2):cli(i,'trust',keys[1-i],'Class fixture peer')
    def daemon(i):
        p=subprocess.Popen(COMMAND+['--data-dir',str(homes[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}','--codex',str(fixture)],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
        processes.append(p);lines=[p.stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1],lines
        return p,lines[-1].strip().split('#')[1]
    _,t0=daemon(0);remote,t1=daemon(1)
    def api(request,foreign=False,local=False):
        i=1 if foreign else 0;token=t1 if foreign else t0
        r=urllib.request.Request(f'http://127.0.0.1:{uis[i]}/api/operation',headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'},data=json.dumps({'peer':keys[0] if local else keys[1],'address':'' if local else f'127.0.0.1:{peers[1]}','request':request}).encode())
        return json.load(urllib.request.urlopen(r,timeout=15))
    def classes(action,foreign=False,**args):return api({'op':'resident_definition','args':{'action':action,**args}},foreign)
    def get(r):return api({'op':'resident_get','id':r})
    def reject(call):
        try:call()
        except urllib.error.HTTPError as e:assert e.code==422,e.code;return
        raise AssertionError('expected a rejected operation')
    def activate(r,directions='RESIDENT_READ'):
        run=api({'op':'resident_activate','id':r,'activation_id':secrets.token_hex(32),'directions':directions});assert run
        ident=run['record']['spec']['id']
        deadline=time.monotonic()+12
        while time.monotonic()<deadline:
            snapshot=api({'op':'get','id':ident})
            if snapshot['status'] in ('completed','failed','interrupted','cancelled'):
                assert snapshot['status']=='completed',snapshot;return snapshot
            time.sleep(.04)
        raise AssertionError('activation did not complete')
    def patch(r,**values):return api({'op':'resident_update','id':r,'patch':{'expected_revision':get(r)['revision'],**values}})
    def apply(r,version,fields,expected=None):return classes('apply',resident_id=r,definition_revision=version,fields=fields,expected_revision=expected if expected is not None else get(r)['revision'])
    def restart():
        global remote,t1
        remote.kill();remote.wait(timeout=5);remote,t1=daemon(1)
    try:
        log=root/'diagnostics.jsonl';api({'op':'log_file','path':str(log)})
        ident=secrets.token_hex(32);spec={'id':ident,'name':'Spec','identity':'CLASS_IDENTITY_ORIGINAL','memory':'CLASS_SEED_ORIGINAL','backend':'codex','cwd':temp,'model':'fixture','auto_activate':False}
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            created=list(pool.map(lambda _:classes('create',spec=spec),range(2)))
        assert created[0]==created[1] and created[0]['revision']==1
        assert classes('get',id=ident)['spec']['memory']==spec['memory']
        assert classes('get',foreign=True,id=ident)==created[0]
        assert classes('list',foreign=True)['definitions'][0]['id']==ident
        original=(homes[1]/'resident-definitions'/ident/'1.json').read_bytes()
        requests=[{'id':secrets.token_hex(32),'definition_id':ident,'definition_revision':1,'overrides':{'name':name}} for name in ['One','Two']]
        a,b=[classes('instantiate',spec=s) for s in requests]
        aid,bid=a['id'],b['id']
        assert aid!=bid and a['thread_id']==aid and b['thread_id']==bid
        assert a['definition']['instantiated_revision']==1 and a['definition']['initial_overrides']==['name']
        assert 'name' not in a['definition']['last_applied_fields']
        assert api({'op':'list'})==[],'instantiation activated a workload'
        standalone=api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'name':'Standalone','cwd':temp,'auto_activate':False}})
        assert 'definition' not in standalone
        reject(lambda:apply(standalone['id'],1,['identity']))
        reject(lambda:classes('instantiate',spec=requests[0]|{'overrides':{'name':'other'}}))
        assert classes('instantiate',foreign=True,spec=requests[0])==a
        reject(lambda:classes('instantiate',spec={'id':secrets.token_hex(32),'definition_id':ident,'definition_revision':0}))
        reject(lambda:classes('instantiate',spec={'id':secrets.token_hex(32),'definition_id':ident,'definition_revision':1,'overrides':{'id':secrets.token_hex(32)}}))
        arun=activate(aid,'RESIDENT_WRITE');brun=activate(bid)
        assert arun['record']['spec']['thread_id']==aid and brun['record']['spec']['thread_id']==bid
        assert arun['record']['resident']['definition']['instantiated_revision']==1
        assert get(aid)['memory']=='fixture durable memory' and get(bid)['memory']==spec['memory']
        reject(lambda:api({'op':'thread_result','id':arun['record']['spec']['id']}))
        before_a,before_b=get(aid),get(bid)
        second=classes('update',id=ident,expected_revision=1,patch={'identity':'CLASS_IDENTITY_SECOND','memory':'CLASS_SEED_SECOND','auto_activate':True})
        assert second['revision']==2 and get(aid)==before_a and get(bid)==before_b
        assert (homes[1]/'resident-definitions'/ident/'1.json').read_bytes()==original
        assert classes('create',spec=spec)==created[0]
        assert classes('get',id=ident,revision=1)==created[0]
        assert len(classes('history',id=ident,limit=1)['revisions'])==1
        reject(lambda:classes('update',id=ident,expected_revision=1,patch={'identity':'stale'}))
        c=classes('instantiate',spec={'id':secrets.token_hex(32),'definition_id':ident,'definition_revision':2,'overrides':{'name':'Three'}})
        assert c['memory']=='CLASS_SEED_SECOND' and c['auto_activate'] is True and len(api({'op':'list'}))==2
        evolved=apply(aid,2,['identity'])
        assert evolved['memory']=='fixture durable memory' and evolved['name']=='One' and evolved['auto_activate'] is False
        assert evolved['identity']=='CLASS_IDENTITY_SECOND' and evolved['definition']['last_applied_fields']['identity']==2
        assert get(bid)==before_b
        reject(lambda:apply(aid,2,['memory'],expected=before_a['revision']))
        assert classes('instantiate',spec=requests[0])==get(aid),'creation retry reverted an evolved instance'
        reset=apply(aid,2,['memory']);assert reset['memory']=='CLASS_SEED_SECOND'
        patch(aid,memory='evolved '*40)
        new_cwd=root/'second';new_cwd.mkdir()
        third=classes('update',id=ident,expected_revision=2,patch={'identity':'v3','memory':'seed3','quota_bytes':32,'cwd':str(new_cwd),'model':'changed-fixture'})
        old=get(aid);reject(lambda:apply(aid,3,['identity','quota_bytes']));assert get(aid)==old,'quota rejection partly applied a definition'
        reject(lambda:apply(aid,3,['identity','model']));assert get(aid)==old,'runtime pin rejection partly applied fields'
        reject(lambda:apply(aid,3,[]));reject(lambda:apply(aid,3,['identity','identity']));reject(lambda:apply(aid,3,['unknown']))
        idle=apply(c['id'],3,['cwd','model']);assert idle['cwd']==str(new_cwd) and idle['model']=='changed-fixture'
        assert get(bid)['definition']['last_applied_fields']['identity']==1
        assert [v['revision'] for v in classes('history',id=ident)['revisions']]==[1,2,3]
        # Each instance has an independent inbox and explicit correspondence, rather than workload responses.
        mail=api({'op':'mailbox_send','client_id':keys[0],'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':bid},'subject':'Class mail proof','body':'REPLY once'}})
        assert not api({'op':'resident_inbox','id':aid})['messages']
        assert len(api({'op':'resident_inbox','id':bid})['messages'])==1
        activate(bid)
        deadline=time.monotonic()+20
        while api({'op':'mailbox_status'})['network_pending']:
            assert time.monotonic()<deadline;time.sleep(.1)
        replies=api({'op':'client_inbox','client_id':keys[0]},local=True)['messages'];assert len(replies)==1 and replies[0]['in_reply_to']==mail['id']
        # Legacy files need no migration, and no instance lifecycle depends on class storage availability.
        legacy=homes[1]/'residents'/(standalone['id']+'.json');record=json.loads(legacy.read_text());record.pop('instantiation',None);legacy.write_text(json.dumps(record))
        restart();assert get(aid)['memory']=='evolved '*40 and 'definition' not in get(standalone['id'])
        assert classes('instantiate',spec=requests[0])==get(aid)
        (homes[1]/'resident-definitions'/ident/'3.json').write_text('{broken')
        restart();assert classes('list')['issues'];reject(lambda:classes('get',id=ident))
        assert len(classes('instances',id=ident))==3
        assert classes('instantiate',spec=requests[0])==get(aid),'retry depended on a damaged definition'
        activate(bid);assert get(bid)['memory']==spec['memory']
        shutil.rmtree(homes[1]/'resident-definitions');(homes[1]/'resident-definitions').write_text('unavailable definition registry')
        restart();assert classes('list')['issues'];activate(bid)
        assert get(aid)['memory']=='evolved '*40
        # Ordinary Boxen Codex conversations can also be promoted without an import.
        ordinary_id=secrets.token_hex(32)
        api({'op':'start','spec':{'id':ordinary_id,'thread_id':ordinary_id,'backend':'codex','cwd':temp,'model':'fixture','prompt':'Ordinary conversation before promotion'}})
        for _ in range(200):
            prior=api({'op':'get','id':ordinary_id})
            if prior['status']=='completed':break
            time.sleep(.02)
        assert prior['status']=='completed',prior
        promoted=api({'op':'resident_promote','spec':{'thread_id':ordinary_id,'name':'Promoted ordinary fixture','auto_activate':False}})
        assert promoted['id']==ordinary_id and promoted['promotion']['backend_thread_id']==prior['backend_thread']
        continued=activate(ordinary_id,'RESIDENT_WRITE')
        assert continued['backend_thread']==prior['backend_thread']
        assert get(ordinary_id)['memory']=='fixture durable memory'
        text=log.read_text();assert 'resident_definition.created' in text and 'resident_definition.instantiated' in text and 'resident.definition_applied' in text
        assert 'CLASS_SEED_ORIGINAL' not in text and 'CLASS_IDENTITY_ORIGINAL' not in text
        print(json.dumps({'resident_classes':'passed','instances':3,'independent_memory_and_threads':True,'manual_field_application':True,'no_automatic_propagation':True,'immutable_revisions':True,'idempotent_creation':True,'runtime_pins':True,'quota_atomicity':True,'definition_failure_isolation':True,'backend':'controlled Codex fixture'}))
    finally:
        for process in processes:
            if process.poll() is None:process.terminate()
        for process in processes:
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:process.kill();process.wait()
