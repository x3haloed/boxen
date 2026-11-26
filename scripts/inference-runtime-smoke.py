#!/usr/bin/env python3
"""Two real paired daemons; controlled HTTP inference, actual tools/journals/mailboxes."""
import http.server
from smoke_helpers import fake_codex
from smoke_helpers import python_shell
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

root=pathlib.Path(__file__).resolve().parent.parent
binary=pathlib.Path(sys.argv[1] if len(sys.argv)>1 else root/'target/release/boxen').resolve()
command=(['/bin/bash'] if binary.suffix=='.com' else [])+[str(binary)]
observed=[]
def frame(delta=None,finish=None):return {'id':'fixture-response','choices':[{'index':0,'delta':delta or {},'finish_reason':finish}]}
class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self,*_):pass
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert 'Authorization' not in self.headers
        observed.append(body)
        messages=body['messages'];last_user=max(i for i,m in enumerate(messages) if m['role']=='user')
        prompt=messages[last_user]['content'];after=messages[last_user+1:]
        tools=[m for m in after if m['role']=='tool']
        if prompt=='QUIET_HEADERS':time.sleep(130)
        self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('X-Provider-Name','fixture-only');self.send_header('X-Generation-Id','fixture-generation');self.end_headers()
        def emit(value):
            self.wfile.write(('data: '+json.dumps(value)+'\n\n').encode());self.wfile.flush()
        def call(name,args,finish='tool_calls'):
            emit(frame({'reasoning':'Visible fixture thinking.'}))
            encoded=args if isinstance(args,str) else json.dumps(args)
            emit(frame({'tool_calls':[{'index':0,'id':'call-fixture','type':'function','function':{'name':name,'arguments':encoded[:len(encoded)//2]}}]}))
            emit(frame({'tool_calls':[{'index':0,'function':{'arguments':encoded[len(encoded)//2:]}}]}))
            emit(frame(finish=finish))
        def text(value):
            emit(frame({'content':value[:len(value)//2],'future_field':{'kept':True},'reasoning_details':[{'index':0,'type':'reasoning.encrypted','data':'fixture-opaque','id':'opaque-1'}]}))
            time.sleep(.12);emit(frame({'content':value[len(value)//2:]}));emit(frame(finish='stop'))
        try:
            if prompt in {'LONG_STREAM', 'QUIET_STREAM', 'ABANDON_STREAM'}:
                emit(frame({'content':'begin '}))
                if prompt=='ABANDON_STREAM':
                    time.sleep(8)
                    return
                if prompt=='QUIET_STREAM':
                    time.sleep(130)
                else:
                    for _ in range(13):
                        time.sleep(10)
                        emit(frame({'reasoning':'working'}))
                emit(frame({'content':'completed'}))
                emit(frame(finish='stop'))
                self.wfile.write(b'data: [DONE]\n\n')
                self.wfile.flush()
                return
            if prompt=='SLOW_HTTP':time.sleep(10);return
            if prompt.startswith(('Mailbox activation.','Message activation.')):
                batch=json.loads(prompt.split('\n',1)[1]);direction=batch['directions']
                reply=next((m for m in batch['messages'] if m['body'].startswith('REPLY')),None)
                if direction=='SCHEDULE_FIXTURE' and not tools:
                    call('boxen_schedule', {'action':'create','spec':{'id':secrets.token_hex(32),'name':'Inference fixture wake','at_ms':int(time.time()*1000)+900,'directions':'Inference scheduled follow-up'}})
                elif direction=='SCHEDULE_FIXTURE':
                    assert json.loads(tools[-1]['content'])['handoff']['run_id']
                    text('PRIVATE_TIMING_ACCEPTED')
                elif direction=='Inference scheduled follow-up':
                    assert batch['schedule']['handoff']['run_id']
                    text('PRIVATE_SCHEDULED_FOLLOWUP')
                elif direction=='BURST' and not tools:
                    assert body['provider']=={'only':['fixture-only'],'order':['fixture-only'],'allow_fallbacks':False,'require_parameters':True},body['provider']
                    assert body['max_tokens']==8192
                    address=batch['messages'][0]['reply_to']
                    calls=[]
                    for i in range(4):
                        generic=i%2==1
                        args={'action':'send','to':{'instance':'boxen','conversation':'client:'+address['machine']+':'+address['id']},'subject':'Budget proof','body':'BUDGET_'+str(i)} if generic else {'to':address,'subject':'Budget proof','body':'BUDGET_'+str(i)}
                        calls.append({'index':i,'id':'budget-'+str(i),'type':'function','function':{'name':'boxen_messages' if generic else 'boxen_send_message','arguments':json.dumps(args)}})
                    emit(frame({'tool_calls':calls}));emit(frame(finish='tool_calls'))
                elif direction=='BURST':
                    assert len(tools)==4
                    outputs=[json.loads(m['content']) for m in tools]
                    assert all('error' in o for o in outputs[2:]),outputs
                    text('PRIVATE_BUDGET_COMPLETE')
                elif direction=='UNLIMITED':
                    assert 'max_tokens' not in body,body
                    if len(tools)<20:call('boxen_send_message',{'to':batch['messages'][0]['reply_to'],'subject':'Unbounded proof','body':'UNLIMITED_'+str(len(tools))})
                    else:text('UNLIMITED_COMPLETE')
                elif direction=='CONTINUITY' and len(tools)==0:call('boxen_continuity',{'action':'read'})
                elif direction=='CONTINUITY' and len(tools)==1:
                    current=json.loads(tools[-1]['content']);call('boxen_continuity',{'action':'write','expected_revision':current['revision'],'memory':'inference-memory-proof'})
                elif direction.startswith('THREAD:') and not tools:
                    call('boxen_thread_request',{'request_id':direction.split(':')[1],'prompt':'CHILD_REQUEST'})
                elif direction=='INTERRUPT_RESIDENT' and tools:emit(frame({'content':'interrupted private text'}));return
                elif reply and not tools:call('boxen_send_message',{'to':reply['reply_to'],'subject':'Selective reply','body':'EXPLICIT_DELIVERY','in_reply_to':reply['id']})
                else:text('PRIVATE_ONLY' if direction!='EMPTY' else '')
            elif prompt=='AGENT_WORK':
                exposed={t['function']['name'] for t in body['tools']}
                assert {'boxen_machines','boxen_work','boxen_thread_files'} <= exposed
                if not tools:call('boxen_machines',{'machine':'local'})
                elif len(tools)==1:
                    assert json.loads(tools[-1]['content'])['machines'][0]['reachable']
                    call('boxen_work',{'action':'request','request_id':'built-in-child','backend':'openrouter','model':'fixture','prompt':'CHILD_REQUEST'})
                else:
                    value=json.loads(tools[-1]['content'])
                    if value.get('done'):
                        assert value['response']=='CHILD_RESPONSE',value
                        text('BUILT_IN_WORK_COMPLETE')
                    else:call('boxen_work',{'action':'read','request_id':'built-in-child','after':value.get('next_after',0),'wait_ms':20000})
            elif prompt.startswith('HANDOFF_REQUEST '):
                if not tools: call('boxen_work', {'action':'handoff','request_id':prompt.split(' ',1)[1],'prompt':'CHILD_REQUEST'})
                else:
                    assert json.loads(tools[-1]['content'])['acceptance']=='accepted'
                    text('TRANSFER_ACCEPTED')
            elif prompt=='CHILD_REQUEST':text('CHILD_RESPONSE')
            elif prompt=='QUIET_HEADERS':text('delayed headers')
            elif prompt=='FOLLOWUP':
                assert any(m.get('reasoning_details') for m in messages if m['role']=='assistant')
                assert any(m.get('future_field')=={'kept':True} for m in messages)
                text('FOLLOWUP_RESPONSE')
            elif prompt=='RECOVER':
                assert any(m.get('role')=='tool' for m in messages)
                text('RECOVERED_WITHOUT_REPLAY')
            elif prompt=='WAIT_EFFECT' and not tools:call('boxen_shell',{'command':python_shell("import time; open('effect.txt','a').write('once'); time.sleep(.15); print('command-output',end='')")})
            elif prompt=='WAIT_EFFECT':text('ORDINARY_RESPONSE')
            elif prompt=='INTERRUPT_AFTER_EFFECT' and not tools:call('boxen_shell',{'command':python_shell("open('uncertain.txt','a').write('once')")})
            elif prompt=='INTERRUPT_AFTER_EFFECT':emit(frame({'content':'unaccepted partial'}));return
            elif prompt=='TRUNCATED':call('boxen_shell',{'command':'touch forbidden.txt'},finish='length')
            elif prompt=='TRUNCATED_RECOVER':
                if body.get('reasoning')=={'effort':'low'}:text('TRUNCATED_RECOVERED')
                else:call('boxen_shell',{'command':'touch forbidden.txt'},finish='length')
            elif prompt=='RUNAWAY_LOOP':
                if body.get('reasoning')=={'effort':'low'}:text('LOOP_RECOVERED')
                else:
                    for _ in range(120):
                        emit(frame({'content':'','reasoning':'Hmm. Hmm. Hmm. Hmm. Hmm.\n\n'}))
                        time.sleep(.01)
                    emit(frame(finish='length'))
            elif prompt=='RUNAWAY_LIMIT':
                if body.get('reasoning')=={'effort':'low'}:text('LIMIT_RECOVERED')
                else:
                    for i in range(60):
                        emit(frame({'content':'','reasoning':f'deliberation-segment-{i:03d}-distinct-token-sequence '}))
                        time.sleep(.01)
                    emit(frame(finish='length'))
            elif prompt=='SLOW_SHELL' and not tools:call('boxen_shell',{'command':python_shell("import subprocess,sys; child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)']); open('descendant.pid','w').write(str(child.pid)); open('started.txt','w').write('started'); child.wait()")})
            elif prompt=='BAD_ARGUMENTS' and not tools:call('boxen_shell','{"command":')
            elif prompt=='BAD_ARGUMENTS':text('ARGUMENT_REJECTION_OBSERVED')
            elif prompt=='STEER_ME' and not tools:call('boxen_shell',{'command':python_shell("import time; time.sleep(.5); print('ready',end='')")})
            elif prompt=='STEER_ME':text('UNSTEERED')
            elif prompt=='STEERING_TEXT':text('STEERED_RESPONSE')
            elif prompt=='EMPTY':text('')
            else:text('GENERIC_RESPONSE')
            self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()
        except (BrokenPipeError,ConnectionResetError):pass
provider=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider)
threading.Thread(target=provider.serve_forever,daemon=True).start()
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
with tempfile.TemporaryDirectory(prefix='boxen-runtime-') as temp:
    directory=pathlib.Path(temp);data=[directory/'a',directory/'b'];processes=[];tokens=[]
    peers=[port(),port()];uis=[port(),port()]
    def cli(i,*args):return subprocess.check_output(command+['--data-dir',str(data[i]),*args],text=True,timeout=10).strip()
    keys=[cli(i,'identity') for i in range(2)]
    for i in range(2):cli(i,'trust',keys[1-i],'Other')
    def daemon(i, fixture=True):
        p=subprocess.Popen(command+['--data-dir',str(data[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}',
            '--codex',str(fake_codex())]+(['--openrouter-endpoint',f'http://127.0.0.1:{provider.server_port}/chat'] if fixture else []),stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        processes.append(p);lines=[p.stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1],lines
        return lines[-1].strip().split('#')[1]
    tokens=[daemon(i) for i in range(2)]
    target={'peer':keys[1],'address':f'127.0.0.1:{peers[1]}'}
    def api(op,watch=False,local=False):
        request=urllib.request.Request(f'http://127.0.0.1:{uis[0]}/api/'+('watch' if watch else 'operation'),
            headers={'Authorization':'Bearer '+tokens[0],'Content-Type':'application/json'},data=json.dumps({**({'peer':keys[0],'address':''} if local else target),'request':op}).encode())
        response=urllib.request.urlopen(request,timeout=15)
        return response if watch else json.load(response)
    def start(prompt,thread=None,require=None):
        ident=secrets.token_hex(32);spec={'id':ident,'thread_id':thread or ident,'cwd':temp,'model':'fixture','backend':'openrouter','prompt':prompt}
        if require:
            spec['required_capabilities'] = require
        return api({'op':'start_checked' if require else 'start','spec':spec})
    def collect(run,after=0):
        events=[]
        with api({'id':run['record']['spec']['id'],'after':after},True) as stream:
            for line in stream:
                batch=json.loads(line);assert 'error' not in batch,batch;events+=batch['events']
                if batch['done']:return events,batch['snapshot']
        raise AssertionError('watch ended early')
    def until(run,kind):
        with api({'id':run['record']['spec']['id'],'after':0},True) as stream:
            for line in stream:
                batch=json.loads(line)
                if any(e['kind']==kind for e in batch['events']):return batch
                assert not batch['done'],batch
    def result(run):return api({'op':'thread_result','id':run['record']['spec']['id']})
    def resident(auto=False):return api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'name':'Inference resident','cwd':temp,'backend':'openrouter','model':'fixture','auto_activate':auto}})
    def activate(r,directions=''):return api({'op':'resident_activate','id':r['id'],'activation_id':secrets.token_hex(32),'directions':directions})
    client=secrets.token_hex(32)
    def send(r,body):return api({'op':'mailbox_send','client_id':client,'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':r['id']},'subject':'Test','body':body}})
    def inbox():
        deadline=time.monotonic()+40
        while api({'op':'mailbox_status'})['network_pending']:
            assert time.monotonic()<deadline;time.sleep(.1)
        return api({'op':'client_inbox','client_id':client},local=True)['messages']
    try:
        idle_policy=data[1]/'runtime-policy.json'
        idle_policy.write_text(json.dumps({'inference_idle_timeout_seconds':2}))
        abandoned=start('ABANDON_STREAM')
        idle_policy.unlink()  # The accepted run retains its own policy.
        abandoned_events,abandoned_state=collect(abandoned)
        assert abandoned_state['status']=='interrupted'
        assert any(e['kind']=='inference_frame' and
                   e['data']['event'].get('idle_timeout_seconds')==2 and
                   e['data']['event']['stage']=='timeout' and
                   e['data']['event']['code']=='idle_timeout' for e in abandoned_events)
        print('PASS explicit inactivity policy is captured and recorded on abandonment')
        if '--long-stream' in sys.argv[2:]:
            start_time=time.monotonic()
            productive=start('LONG_STREAM')
            quiet=start('QUIET_STREAM')
            late_headers=start('QUIET_HEADERS')
            quiet_events,ended=collect(quiet)
            assert ended['status']=='completed', ended['status']
            details=[e['data']['detail'] for e in quiet_events if e['kind']=='status']
            assert details.count('Waiting for the inference provider. The run remains active.')==1
            assert details.count('The inference provider resumed responding')==1
            _,ended=collect(productive)
            assert ended['status']=='completed', ended['status']
            assert result(productive)['response']==result(quiet)['response']=='begin completed'
            assert collect(late_headers)[1]['status']=='completed'
            assert result(late_headers)['response']=='delayed headers'
            assert time.monotonic()-start_time >= 130
            print('PASS productive and quiet SSE beyond the former wall-clock limit with visible wait and recovery')
        assert api({'op':'runtime_info'})['backends'][1]['configured']
        log=directory/'runtime-log.jsonl';api({'op':'log_file','path':str(log),'filter':{'targets':['runtime'],'events':['inference.']}})
        run_id=secrets.token_hex(32)
        spec={'id':run_id,'thread_id':run_id,'backend':'openrouter','model':'fixture','cwd':temp,
              'prompt':'WAIT_EFFECT','reasoning_effort':'low'}
        before=len(observed)
        run=api({'op':'start','spec':spec})
        until(run,'reasoning_delta')
        settings={'op':'thread_reasoning','id':run_id}
        prior=api(settings)
        updated=api({**settings,'patch':{'expected_version':prior['version'],'reasoning_effort':'high'}})
        prefix_events,ended=collect(run)
        assert ended['status']=='completed'
        assert [b['reasoning'] for b in observed[before:]]==[{'effort':'low'}]*2
        assert all(b['provider']['require_parameters'] for b in observed[before:])
        initial,tool_followup=observed[before:]
        session_id=f'boxen:{run_id}:workload'
        assert initial['session_id']==tool_followup['session_id']==session_id
        shared=initial['messages'][:-1]
        assert [m['role'] for m in shared]==['system','user']
        assert tool_followup['messages'][:len(shared)]==shared
        assert tool_followup['messages'][-2]['role']=='tool'
        assert initial['tools']==tool_followup['tools']
        for request in (initial,tool_followup):
            tail=request['messages'][-1]
            assert tail['role']=='system'
            assert f'This is Boxen run {run_id}' in tail['content']
            assert 'This input was assembled at' in tail['content']
            assert 'This input was assembled at' not in request['messages'][0]['content']
        assert initial['messages'][-1]!=tool_followup['messages'][-1]
        accepted=[e['data'] for e in prefix_events if e['kind']=='inference_message']
        assert [m['role'] for m in accepted]==['user','assistant','tool','assistant']
        processes[-1].kill();processes[-1].wait(timeout=5);tokens[1]=daemon(1)
        assert api(settings)==updated
        before=len(observed)
        follow=start('FOLLOWUP',thread=run_id)
        assert collect(follow)[1]['status']=='completed'
        resumed=observed[before]['messages']
        assert observed[before]['session_id']==session_id
        assert resumed[:len(tool_followup['messages'])-1]==tool_followup['messages'][:-1]
        assert f"This is Boxen run {follow['record']['spec']['id']}" in resumed[-1]['content']
        print('PASS stable provider-input prefix across tool exchanges and restart with fresh, wire-only run context')
        assert observed[before]['reasoning']=={'effort':'high'}
        assert api({'op':'get','id':run_id})['record']['reasoning_effort']=='low'
        prior=api(settings)
        api({**settings,'patch':{'expected_version':prior['version'],'reasoning_effort':'default'}})
        before=len(observed)
        assert collect(start('FOLLOWUP',thread=run_id))[1]['status']=='completed'
        assert 'reasoning' not in observed[before]
        r=api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'name':'Reasoning resident','cwd':temp,
               'backend':'openrouter','model':'fixture','reasoning_effort':'minimal','auto_activate':False}})
        for effort in ('minimal','max','default'):
            if effort!='minimal':
                r=api({'op':'resident_update','id':r['id'],'patch':{'expected_revision':r['revision'],
                       'reasoning_effort':effort}})
            before=len(observed)
            activation=activate(r)
            assert collect(activation)[1]['status']=='completed'
            assert activation['record']['reasoning_effort']==effort
            assert observed[before]['session_id']==f"boxen:{r['id']}:workload"
            assert observed[before]['session_id']!=session_id
            assert observed[before].get('reasoning')==(None if effort=='default' else {'effort':effort})
        (directory/'effect.txt').unlink()
        print('PASS OpenRouter reasoning wire values, strict routing, active run isolation, restart and default reset')

        coordination=start('AGENT_WORK');assert collect(coordination)[1]['status']=='completed'
        assert result(coordination)['response']=='BUILT_IN_WORK_COMPLETE'
        print('PASS built-in agent discovery and ordinary child work through the shared tool authority')
        ordinary=start('WAIT_EFFECT');early=until(ordinary,'assistant_delta');assert not early['done']
        events,done=collect(ordinary);assert done['status']=='completed',done
        assert result(ordinary)['response']=='ORDINARY_RESPONSE'
        assert (directory/'effect.txt').read_text()=='once'
        replay,_=collect(ordinary,events[len(events)//2]['seq']);assert [e['seq'] for e in replay]==[e['seq'] for e in events if e['seq']>events[len(events)//2]['seq']]
        follow=start('FOLLOWUP',ordinary['record']['spec']['thread_id']);assert collect(follow)[1]['status']=='completed'
        assert result(follow)['response']=='FOLLOWUP_RESPONSE'
        print('PASS remote shell effect, realtime observation, reconnect replay, conventional response and native metadata in follow-up')
        truncated=start('TRUNCATED');trunc_events,trunc_done=collect(truncated)
        assert trunc_done['status']=='interrupted' and not (directory/'forbidden.txt').exists()
        assert [e['data']['role'] for e in trunc_events if e['kind']=='inference_message']==['user']
        assert sum(e['kind']=='inference_exchange_retry' for e in trunc_events)==1
        trunc_follow=start('CHILD_REQUEST',truncated['record']['spec']['thread_id'])
        tf_events,tf_done=collect(trunc_follow)
        assert tf_done['status']=='completed' and result(trunc_follow)['response']=='CHILD_RESPONSE'
        assert not any(e['kind']=='context_projection' for e in tf_events)
        before=len(observed);tr_rec=start('TRUNCATED_RECOVER');tr_events,tr_done=collect(tr_rec)
        assert tr_done['status']=='completed' and result(tr_rec)['response']=='TRUNCATED_RECOVERED'
        assert not (directory/'forbidden.txt').exists()
        assert [e['data']['role'] for e in tr_events if e['kind']=='inference_message']==['user','assistant']
        assert sum(e['kind']=='inference_exchange_retry' for e in tr_events)==1
        assert 'reasoning' not in observed[before] and observed[before+1]['reasoning']=={'effort':'low'}
        assert observed[before]['messages'][:-1]==observed[before+1]['messages'][:-1]
        assert 'reached the output token limit' in observed[before+1]['messages'][-1]['content']
        before=len(observed);loop_run=start('RUNAWAY_LOOP');loop_events,loop_done=collect(loop_run)
        assert loop_done['status']=='completed' and result(loop_run)['response']=='LOOP_RECOVERED'
        assert [e['data']['role'] for e in loop_events if e['kind']=='inference_message']==['user','assistant']
        assert any(e['kind']=='inference_frame' and e['data']['event'].get('type')=='exchange_aborted' and e['data']['event'].get('kind')=='reasoning_repetition' for e in loop_events)
        assert sum(e['kind']=='inference_exchange_retry' for e in loop_events)==1
        assert 'reasoning' not in observed[before] and observed[before+1]['reasoning']=={'effort':'low'}
        assert observed[before]['messages'][:-1]==observed[before+1]['messages'][:-1]
        assert 'repetition loop' in observed[before+1]['messages'][-1]['content']
        health_after_loop=api({'op':'inference_health','offset':0})['incidents']
        assert not any(i['thread_id']==loop_run['record']['spec']['thread_id'] for i in health_after_loop),health_after_loop
        limit_policy=data[1]/'runtime-policy.json'
        limit_policy.write_text(json.dumps({'max_reasoning_bytes':2048}))
        before=len(observed);limit_run=start('RUNAWAY_LIMIT');limit_policy.unlink()
        limit_events,limit_done=collect(limit_run)
        assert limit_done['status']=='completed' and result(limit_run)['response']=='LIMIT_RECOVERED'
        assert any(e['kind']=='inference_frame' and e['data']['event'].get('type')=='exchange_aborted' and e['data']['event'].get('kind')=='reasoning_limit' and e['data']['event'].get('max_reasoning_bytes')==2048 for e in limit_events)
        assert observed[before+1]['reasoning']=={'effort':'low'}
        assert observed[before]['messages'][:-1]==observed[before+1]['messages'][:-1]
        print('PASS runaway reasoning loop/limit abort, unaccepted length truncation, prefix-preserving bounded retry and clean health projection')
        bad=start('BAD_ARGUMENTS');assert collect(bad)[1]['status']=='completed';assert result(bad)['response']=='ARGUMENT_REJECTION_OBSERVED'
        interrupted=start('INTERRUPT_AFTER_EFFECT');assert collect(interrupted)[1]['status']=='interrupted'
        assert (directory/'uncertain.txt').read_text()=='once'
        # Restart the executing daemon. No prior prompt or tool is automatically submitted again.
        processes[-1].kill();processes[-1].wait(timeout=5);tokens[1]=daemon(1)
        recovery=start('RECOVER',interrupted['record']['spec']['thread_id']);assert collect(recovery)[1]['status']=='completed'
        assert (directory/'uncertain.txt').read_text()=='once'
        print('PASS no truncated-call execution, malformed argument rejection, restart continuation and no completed-effect replay')
        for prompt,wait_kind in [('SLOW_HTTP','inference_attempt'),('SLOW_SHELL','tool_started')]:
            run=start(prompt);until(run,wait_kind);begin=time.monotonic();api({'op':'control','id':run['record']['spec']['id'],'action':{'type':'cancel'}})
            assert collect(run)[1]['status']=='cancelled';assert time.monotonic()-begin<3
            if prompt=='SLOW_SHELL':
                recovered=start('RECOVER',run['record']['spec']['thread_id']);recovery_events,done=collect(recovered);assert done['status']=='completed'
                assert any(e['kind']=='context_projection' and e['data']['unknown_tool_results']==1 for e in recovery_events)
        steer=start('STEER_ME',require=['queued_steering']);until(steer,'tool_started');api({'op':'control','id':steer['record']['spec']['id'],'action':{'type':'steer','text':'STEERING_TEXT'}})
        assert collect(steer)[1]['status']=='completed';assert result(steer)['response']=='STEERED_RESPONSE'
        print('PASS cancellation during HTTP/tool execution and steering at the next inference boundary')
        info=api({'op':'runtime_info','run_id':steer['record']['spec']['id']})
        assert info['declared']['id']=='openrouter' and info['requested']['required_capabilities']==['queued_steering']
        assert info['observed']['session']['model']=='fixture'
        assert info['observed']['session']['id']==info['thread_id']

        timer_resident=resident()
        source=api({'op':'resident_activate_checked','id':timer_resident['id'],'activation_id':secrets.token_hex(32),
                    'directions':'SCHEDULE_FIXTURE','required_capabilities':['scheduled_handoff']})
        source_events,timer_source=collect(source);assert timer_source['status']=='completed'
        receipt,=[event['data'] for event in source_events if event['kind']=='schedule_handoff']
        assert receipt['source']['run_id']==source['record']['spec']['id']
        deadline=time.monotonic()+8
        while True:
            job=api({'op':'schedule','args':{'action':'get','id':receipt['schedule_id']}})
            if job['last']:break
            assert time.monotonic()<deadline,job
            time.sleep(.05)
        scheduled=api({'op':'get','id':job['last']['run_id']})
        _,dispatched=collect(scheduled);assert dispatched['status']=='completed'
        assert dispatched['record']['scheduled']['handoff']==receipt['source']
        print('PASS resident timing handoff and original provenance through OpenRouter')
        source_id=secrets.token_hex(32);child_id=secrets.token_hex(32)
        source=api({'op':'start_until_handoff','spec':{'id':source_id,'thread_id':source_id,
                    'cwd':temp,'model':'fixture','backend':'openrouter','prompt':'HANDOFF_REQUEST '+child_id,
                    'completion_handoff':{'machine_id':keys[1],'request_id':child_id}}})
        assert collect(source)[1]['status']=='completed' and result(source)['response']=='TRANSFER_ACCEPTED'
        child=api({'op':'get','id':child_id})
        child_events,done=collect(child)
        assert done['status']=='completed' and done['record']['delegated_from']['run_id']==source_id
        assert any(e['kind']=='handoff_origin' and e['data']==done['record']['delegated_from'] for e in child_events)
        print('PASS OpenRouter completion tied to the requested durable child acceptance')


        r=resident();m1=send(r,'SILENT one');m2=send(r,'REPLY one');m3=send(r,'SILENT two')
        activation=activate(r);assert len(activation['record']['mailbox']['message_ids'])==3
        activation_events, activation_snapshot=collect(activation)
        assert activation_snapshot['status']=='completed'
        sent=next(e['data']['result'] for e in activation_events if e['kind']=='tool_result' and e['data']['tool']=='boxen_send_message')
        assert sent['delivery']['status'] in ('queued','accepted') and sent['delivery']['destination_machine']==keys[0],sent
        deadline=time.monotonic()+5
        while True:
            status=api({'op':'messaging','args':{'action':'status','id':sent['id']}})
            if status['delivery']['status']=='accepted':break
            assert status['delivery']['status']=='queued' and time.monotonic()<deadline,status
            time.sleep(.04)
        assert status['delivery']['accepted_on']==keys[0] and status['delivery']['destination_sequence']>0,status
        assert len(inbox())==1 and inbox()[0]['body']=='EXPLICIT_DELIVERY' and inbox()[0]['in_reply_to']==m2['id']
        silent=activate(r,'EMPTY');assert collect(silent)[1]['status']=='completed';assert len(inbox())==1
        memory=activate(r,'CONTINUITY');events,done=collect(memory);assert done['status']=='completed'
        assert api({'op':'resident_get','id':r['id']})['memory']=='inference-memory-proof'
        child_id=secrets.token_hex(32);request=activate(r,'THREAD:'+child_id);assert collect(request)[1]['status']=='completed'
        children=[v for v in api({'op':'list'}) if v['record']['spec']['id']==child_id];assert len(children)==1
        assert collect(children[0])[1]['status']=='completed' and result(children[0])['response']=='CHILD_RESPONSE'
        empty=start('EMPTY');assert collect(empty)[1]['status']=='interrupted' and result(empty)['response'] is None
        send(r,'REPLY interrupted activation')
        interrupted_resident=activate(r,'INTERRUPT_RESIDENT');assert collect(interrupted_resident)[1]['status']=='interrupted'
        assert len(inbox())==2
        continued=activate(r);assert continued['record']['mailbox']['message_ids']==[]
        assert collect(continued)[1]['status']=='completed' and len(inbox())==2
        print('PASS selective resident mail, private narration, no-send completion, shared continuity and distinct child request/response tools')
        current=api({'op':'resident_get','id':r['id']})
        changed=api({'op':'resident_update','id':r['id'],'patch':{'expected_revision':current['revision'],'model':'fixture-next'}})
        assert changed['identity']==current['identity'] and changed['memory']==current['memory']
        for invalid_model in ['', ' '*3, 'x'*257]:
            try:
                api({'op':'resident_update','id':r['id'],'patch':{'expected_revision':changed['revision'],'model':invalid_model}})
                raise AssertionError('invalid model accepted')
            except urllib.error.HTTPError as error:assert b'1\xe2\x80\x93256' in error.read()
        assert api({'op':'resident_get','id':r['id']})==changed
        try:
            api({'op':'resident_update','id':r['id'],'patch':{'expected_revision':current['revision'],'model':'stale-model'}})
            raise AssertionError('stale model update accepted')
        except urllib.error.HTTPError as error:assert b'revision conflict' in error.read()
        processes[-1].kill();processes[-1].wait(timeout=5);tokens[1]=daemon(1)
        before=len(observed);switched=activate(changed,'EMPTY');switched_events,switched_done=collect(switched)
        assert switched_done['status']=='completed' and observed[before]['model']=='fixture-next'
        assert switched['record']['spec']['thread_id']==r['id']
        assert switched['record']['resident']['model']=='fixture-next'
        assert api({'op':'get','id':activation['record']['spec']['id']})['record']['resident']['model']=='fixture'
        assert any('inference-memory-proof' in json.dumps(m) for m in observed[before]['messages'])
        assert any(m.get('role')=='assistant' and m.get('content')=='PRIVATE_ONLY' for m in observed[before]['messages'])
        assert len(inbox())==2
        try:
            api({'op':'start','spec':{**ordinary['record']['spec'],'id':secrets.token_hex(32),'model':'fixture-next','prompt':'EMPTY'}})
            raise AssertionError('anonymous model pin bypassed')
        except urllib.error.HTTPError as error:assert b'pins a thread' in error.read()
        print('PASS revisioned resident model switch, restart persistence, retained continuity and historical model attribution; anonymous model pin preserved')
        assert any('inference.' in line for line in log.read_text().splitlines())
        processes[-1].kill();processes[-1].wait(timeout=5);tokens[1]=daemon(1,fixture=False)
        unconfigured=resident();before=len(observed);missing=activate(unconfigured,'EMPTY')
        missing_events,missing_state=collect(missing);assert missing_state['status']=='interrupted' and len(observed)==before
        assert any(e['kind']=='status' and 'credential' in e['data'].get('detail','') for e in missing_events)
        processes[-1].kill();processes[-1].wait(timeout=5);tokens[1]=daemon(1)
        available=activate(unconfigured,'EMPTY');assert collect(available)[1]['status']=='completed'
        print('PASS idle resident creation without credentials, legible failed activation and continued availability after configuration recovery')
        print('PASS structured opt-in inference diagnostics; existing daemon/journal authority retained')
        policy=data[1]/'runtime-policy.json'
        setting={'providers':['fixture-only'],'allow_fallbacks':False,'max_output_tokens':8192,'max_message_sends':2,'max_exchanges':16}
        policy.write_text(json.dumps(setting))
        limited=resident();send(limited,'BUDGET_INPUT');before=len(inbox())
        burst=activate(limited,'BURST');assert burst['record']['runtime_policy']==setting
        events,done=collect(burst);assert done['status']=='completed',done
        assert len(inbox())==before+2
        assert sum(e['kind']=='message_send_reserved' for e in events)==2
        assert sum(e['kind']=='message_send_limited' for e in events)==1
        headers=[e for e in events if e['kind']=='inference_frame' and e['data']['event'].get('type')=='response_headers']
        assert headers and headers[0]['data']['event']['provider']=='fixture-only' and headers[0]['data']['event']['generation_id']=='fixture-generation',headers
        processes[-1].kill();processes[-1].wait(timeout=5);tokens[1]=daemon(1)
        replay,_=collect(burst);assert replay==events
        send(limited,'BUDGET_INPUT');again=activate(limited,'BURST');assert collect(again)[1]['status']=='completed'
        assert len(inbox())==before+4
        unbounded={**setting,'max_output_tokens':None,'max_message_sends':None,'max_exchanges':None}
        policy.write_text(json.dumps(unbounded));send(limited,'UNLIMITED_INPUT');before_unlimited=len(inbox())
        unlimited=activate(limited,'UNLIMITED');events,done=collect(unlimited)
        assert done['status']=='completed',done
        assert len(inbox())==before_unlimited+20
        assert sum(e['kind']=='message_send_reserved' for e in events)==20
        assert not any(e['kind']=='message_send_limited' for e in events)
        policy.write_text(json.dumps({**unbounded,'max_exchanges':2}));send(limited,'UNLIMITED_INPUT')
        finite=activate(limited,'UNLIMITED');events,done=collect(finite)
        assert done['status']=='interrupted' and any(e['kind']=='status' and '2-exchange budget' in e['data'].get('detail','') for e in events),done
        print('PASS null caps omit output limit and complete 21 exchanges/20 sends; explicit finite exchange budget stays effective')
        policy.write_text('{broken')
        try:start('POLICY_REJECTION')
        except urllib.error.HTTPError as e:assert e.code==422
        else:raise AssertionError('invalid operator policy was silently ignored')
        policy.unlink()
        processes[-1].kill();processes[-1].wait(timeout=5);tokens[1]=daemon(1)
        assert collect(start('POLICY_RECOVERED'))[1]['status']=='completed'
        print('PASS captured provider pin/fallback/output settings, bounded legacy/generic mail attempts, private continuation, producer metadata, restart replay, fresh-run budget and configuration recovery')

    finally:
        for p in processes:
            if p.poll() is None:p.kill()
            p.wait(timeout=5)
provider.shutdown()
