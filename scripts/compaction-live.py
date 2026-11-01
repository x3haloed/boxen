#!/usr/bin/env python3
"""Paid OpenRouter checks of repeated cuts against invented non-leading continuity tasks.

Usage: compaction-live.py [BINARY] [MODEL]. Uses only temporary paired identities.
No Aster/Watch state is accessed. Reports raw outcomes separately from exact retention.
"""
import json
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import urllib.request

binary=pathlib.Path(sys.argv[1] if len(sys.argv)>1 else 'target/release/boxen').resolve()
command=(['/bin/bash'] if binary.suffix=='.com' else [])+[str(binary)]
credential=pathlib.Path.home()/'.boxen/credentials/openrouter.key'
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
with tempfile.TemporaryDirectory(prefix='boxen-runtime-live-') as temp:
    root=pathlib.Path(temp);paths=[root/'a',root/'b'];processes=[];tokens=[];peers=[port(),port()];uis=[port(),port()]
    def cli(i,*args):return subprocess.check_output(command+['--data-dir',str(paths[i]),*args],text=True,timeout=10).strip()
    keys=[cli(i,'identity') for i in range(2)]
    for i in range(2):cli(i,'trust',keys[1-i],'Other')
    try:
        for i in range(2):
            p=subprocess.Popen(command+['--data-dir',str(paths[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}','--openrouter-key-file',str(credential)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            processes.append(p);lines=[p.stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1],lines;tokens.append(lines[-1].strip().split('#')[1])
        target={'peer':keys[1],'address':f'127.0.0.1:{peers[1]}'}
        def api(op,watch=False):
            request=urllib.request.Request(f'http://127.0.0.1:{uis[0]}/api/'+('watch' if watch else 'operation'),data=json.dumps({**target,'request':op}).encode(),headers={'Authorization':'Bearer '+tokens[0],'Content-Type':'application/json'})
            response=urllib.request.urlopen(request,timeout=150)
            return response if watch else json.load(response)
        def collect(run):
            events=[]
            with api({'id':run['record']['spec']['id'],'after':0},True) as stream:
                for line in stream:
                    batch=json.loads(line);assert 'error' not in batch,batch;events+=batch['events']
                    if batch['done']:
                        if batch['snapshot']['status']!='completed':
                            evidence=pathlib.Path('work/compaction');evidence.mkdir(parents=True,exist_ok=True)
                            path=evidence/('failed-live-'+run['record']['spec']['id']+'.jsonl');path.write_text(''.join(json.dumps(e)+'\n' for e in events));path.chmod(0o600)
                        assert batch['snapshot']['status']=='completed',(batch['snapshot'],[e['data'] for e in events if e['kind']=='status'][-1:])
                        return events
            raise AssertionError('stream ended early')
        model=sys.argv[2] if len(sys.argv)>2 else 'openai/gpt-4.1-mini'
        reports=[]
        for mode in ('verbatim','summary'):
            thread=secrets.token_hex(32);tag=secrets.token_hex(4)
            settings={'mode':mode,'budget_bytes':65536,'pin_quota_bytes':8192,'mask_after':2}
            all_events=[];event_runs={}
            def execute(prompt,first=False):
                ident=secrets.token_hex(32)
                spec={'id':ident,'thread_id':thread,'backend':'openrouter','cwd':temp,'model':model,'prompt':prompt}
                if first:spec['context']=settings
                run=api({'op':'start','spec':spec});events=collect(run);all_events.extend(events);event_runs[ident]=events
                return ident,api({'op':'thread_result','id':ident})['response']
            execute(f'This is a disposable continuity test. Your outstanding commitment is to send Nara an inspection report for valve {tag} by Friday. A short acknowledgement is enough.',True)
            execute('Nara subsequently said "no rush on Friday". There is no replacement deadline and no explicit cancellation of the inspection-report commitment. A short acknowledgement is enough.')
            def context(action,**fields):return api({'op':'context','thread_id':thread,'args':{'action':action,**fields}})
            original=context('read',limit=4)['entries']
            if mode=='verbatim':
                context('pin',id=secrets.token_hex(32),entries=[e['id'] for e in original if e['message']['role']=='user'])
            for n in range(6):
                # Unique disposable bulk, so repetitions/cache do not stand in for context selection.
                noise=' '.join(secrets.token_hex(12) for _ in range(480))
                execute(f'Unrelated resolved record batch {n}. This is synthetic bulk with no new obligations or instructions. A short acknowledgement is enough.\n'+noise)
            ident,response=execute('Plan the next work session. What unfinished commitments constrain that plan, and what remains uncertain? Please verify against original history using boxen_history, and include an exact quotation for any later qualification rather than relying only on earlier interpretations.')
            evidence=pathlib.Path(__file__).resolve().parent.parent/'work/compaction/live-evidence';evidence.mkdir(parents=True,exist_ok=True)
            evidence_file=evidence/(ident+'.jsonl');evidence_file.write_text(''.join(json.dumps(event)+'\n' for event in event_runs[ident]));evidence_file.chmod(0o600)
            boundary=context('status')['boundary']
            assert boundary and boundary['source_count']>len(boundary['selected']),boundary
            assert any(e['kind']=='context_boundary' for e in all_events)
            helper=[e for e in all_events if e['kind']=='context_summary_started']
            if mode=='verbatim':assert not helper
            else:assert helper and boundary['summary']
            preserved=tag in response and 'nara' in response.lower() and 'no rush on friday' in response.lower()
            exposed=[];direct=[]
            for event in event_runs[ident]:
                if event['kind']!='context_exposure':continue
                for entry in original:
                    if entry['message']['role']=='user' and entry['id'] in event['data']['selected'] and entry['id'] not in event['data']['masked']:
                        direct.append(entry['message']['content'])
            for index,event in enumerate(event_runs[ident]):
                if event['kind']!='inference_message' or event['data'].get('role')!='tool':continue
                content=event['data'].get('content','')
                try:payload=json.loads(content)
                except ValueError:continue
                if not isinstance(payload.get('entries'),list):continue
                source_id=ident+':'+str(event['seq'])
                later=next((e['data'] for e in event_runs[ident][index+1:] if e['kind']=='context_exposure'),None)
                if later and source_id in later['selected'] and source_id not in later['masked']:
                    exposed.extend(entry["message"]["content"] for entry in payload["entries"] if entry.get("message",{}).get("role")=="user" and isinstance(entry["message"].get("content"),str))
            recall_available=any(tag in text for text in exposed) and any('no rush on Friday' in text for text in exposed)
            originals_present=any(tag in text for text in direct) and any('no rush on Friday' in text for text in direct)
            exact=context('read',id=original[0]['id'],limit=1)['entries'][0]==original[0]
            assert exact
            usage=[e['data']['usage'] for e in all_events if e['kind']=='inference_exchange_completed']
            reports.append({'mode':mode,'model':model,'runs':9,'source_entries':context('status')['entries'],'cut_count':sum(e['kind']=='context_boundary' for e in all_events),'summary_calls':len(helper),'source_exact':exact,'known_commitment_mentioned':preserved,'original_recall_exposed_unmasked':recall_available,'originals_directly_exposed_unmasked':originals_present,'expected_tag':tag,'response':response,'history_tool_calls':sum(e['kind']=='tool_started' and e['data'].get('tool')=='boxen_history' for e in all_events),'total_tokens':sum(u.get('total_tokens',0) for u in usage),'reported_cost':sum(u.get('cost',0) for u in usage)})
            print(json.dumps(reports[-1]),flush=True)
            assert preserved and (recall_available or originals_present), 'Original qualification/recall delivery failed; keyword presence alone is insufficient'
    finally:
        for p in processes:
            if p.poll() is None:p.kill()
            p.wait(timeout=5)
