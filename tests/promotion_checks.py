"""Promotion-specific checks share the paired-daemon/import fixture setup."""
import concurrent.futures
import json
import secrets
import time
import urllib.error
import urllib.request

def check_promotion(g):
    op=g['operation'];reject=g['rejected'];ident=g['ident'];workspace=g['workspace'];native=g['NATIVE'];log=g['log']
    request={'thread_id':ident,'name':'Promoted fixture','identity':'PROMOTED_IDENTITY','memory':'Initial independent memory','auto_activate':False}
    def promote(spec=request,owner=0):return op(owner,{'op':'resident_promote','spec':spec})
    assert op(1,{"op":"get","id":ident})["record"]["owner"]==g["keys"][0]
    reject(lambda:promote({**request,'thread_id':secrets.token_hex(32)}))
    reject(lambda:promote({**request,'quota_bytes':1}))
    assert op(0,{'op':'resident_list'})==[]
    schedule=secrets.token_hex(32)
    op(0,{'op':'schedule','args':{'action':'create','spec':{'id':schedule,'name':'Old anonymous schedule','target':{'type':'thread','id':ident},'directions':'Historical scheduler directions','at_ms':int(time.time()*1000)+86400000}}})
    reject(promote)
    op(0,{'op':'schedule','args':{'action':'update','id':schedule,'patch':{'expected_revision':1,'enabled':False}}})
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        residents=list(pool.map(lambda _:promote(),range(2)))
    resident=residents[0];assert resident==residents[1] and promote(owner=1)==resident
    cli_resident=json.loads(g['cli'](0,'resident-promote',f"127.0.0.1:{g['peers'][1]}",g['keys'][1],json.dumps(request)).stdout)
    assert cli_resident==resident
    assert resident['id']==ident and resident['thread_id']==ident
    assert resident['promotion']['backend_thread_id']==native and len(op(0,{'op':'list'}))==1
    assert len(op(0,{'op':'resident_list'}))==1
    assert op(0,{'op':'client_inbox','client_id':g['keys'][0]},target=0)['messages']==[]
    assert all(r.get('method') not in ('thread/resume','thread/start','turn/start') for r in log()),'promotion executed a model turn'
    reject(lambda:promote({**request,'name':'conflicting retry'}))
    reject(lambda:op(0,{'op':'start','spec':{'id':secrets.token_hex(32),'thread_id':ident,'backend':'codex','cwd':str(workspace),'model':'fixture','prompt':'Direct resident bypass'}}))
    reject(lambda:op(0,{'op':'schedule','args':{'action':'create','spec':{'id':secrets.token_hex(32),'name':'Wrong target type','target':{'type':'thread','id':ident},'directions':'obsolete type','at_ms':int(time.time()*1000)+86400000}}}))
    diagnostics=g['base']/'promotion-diagnostics.jsonl'
    g['cli'](0,'log-file',f"127.0.0.1:{g['peers'][1]}",g['keys'][1],str(diagnostics),'--level','debug','--events','resident_bridge,resident.promotion')
    print('PASS explicit/idempotent promotion retains thread identity, starts no inference, preserves history, and permits shared operators and rejects configuration conflicts')

    def activate(directions=''):
        r=op(0,{'op':'resident_activate','id':ident,'activation_id':secrets.token_hex(32),'directions':directions})
        for _ in range(300):
            s=op(0,{'op':'get','id':r['record']['spec']['id']})
            if s['status'] in ('completed','failed','interrupted','cancelled'):return s
            time.sleep(.02)
        raise AssertionError('controlled activation did not complete')
    def send(body):return op(0,{'op':'mailbox_send','client_id':g['keys'][0],'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':ident},'subject':'Promoted fixture input','body':body}})
    incoming=send('REPLY RESIDENT_WRITE')
    active=activate();assert active['status']=='completed',active
    assert active['backend_thread']==native
    assert op(0,{'op':'resident_get','id':ident})['memory']=='fixture durable memory'
    deadline=time.monotonic()+20
    while op(0,{'op':'mailbox_status'})['network_pending']:
        assert time.monotonic()<deadline;time.sleep(.1)
    reply=op(0,{'op':'client_inbox','client_id':g['keys'][0]},target=0)['messages'];assert len(reply)==1 and reply[0]['in_reply_to']==incoming['id']
    assert promote()['memory']=='fixture durable memory','retry reset evolved continuity'
    reject(lambda:op(0,{'op':'thread_result','id':active['record']['spec']['id']}))
    events=[json.loads(line) for line in g['cli'](0,'watch',f"127.0.0.1:{g['peers'][1]}",g['keys'][1],active['record']['spec']['id'],'--once').stdout.splitlines()]
    assert any(e['kind']=='resident_bridge_ready' for e in events)
    assert any(e['kind']=='resident_bridge_approval_policy' and e['data']=={'server':'boxen_resident','default_tools_approval_mode':'approve','scope':'activation'} for e in events)
    assert not any(e['kind']=='backend_session_rebuilt' for e in events)
    assert any(e['kind']=='resident_bridge_result' for e in events)
    resumed=[r for r in log() if r.get('method')=='thread/resume']
    assert resumed and all(r['params'].get('excludeTurns') is True for r in resumed)
    endpoint=resumed[-1]['params']['config']['mcp_servers.boxen_resident']['url']
    try:
        urllib.request.urlopen(urllib.request.Request(endpoint,data=b'{"jsonrpc":"2.0","id":1,"method":"tools/list"}',headers={'Content-Type':'application/json'}),timeout=2)
        raise AssertionError('ended bridge still accepted requests')
    except (urllib.error.URLError,ConnectionError,OSError):pass
    g['stop'](1);g['start'](1)
    assert promote()['memory']=='fixture durable memory'
    send('RESIDENT_READ')
    quiet=activate();assert quiet['status']=='completed' and quiet['backend_thread']==native,quiet
    assert len(op(0,{'op':'client_inbox','client_id':g['keys'][0]},target=0)['messages'])==1,'private narration became outgoing mail'
    assert all('developerInstructions' not in r['params'] for r in log() if r.get('method')=='thread/resume')
    assert all(r.get('method')!='thread/start' for r in log())
    records=[json.loads(line) for line in diagnostics.read_text().splitlines()]
    assert any(r['event']=='resident_bridge.tool_completed' for r in records)
    assert any(r['event']=='resident_bridge.request_rejected' for r in records)
    assert 'Initial independent memory' not in diagnostics.read_text()
    (workspace/'fixture-forget-session').touch()
    failed=activate();assert failed['status']=='failed',failed
    assert all(r.get('method')!='thread/start' for r in log())
    print('PASS same-native-thread MCP tools, independent memory, explicit/selective replies, private narration, retries, graceful HTTP rejection with bounded stalled-body recovery, bridge lifetime, restart and visible resume failure')
