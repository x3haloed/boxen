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
            if prompt=='SLOW_HTTP':time.sleep(10);return
            if prompt.startswith(('Mailbox activation.','Message activation.')):
                batch=json.loads(prompt.split('\n',1)[1]);direction=batch['directions']
                reply=next((m for m in batch['messages'] if m['body'].startswith('REPLY')),None)
                if direction=='BURST' and not tools:
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
            elif prompt=='CHILD_REQUEST':text('CHILD_RESPONSE')
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
    def start(prompt,thread=None):
        ident=secrets.token_hex(32);spec={'id':ident,'thread_id':thread or ident,'cwd':temp,'model':'fixture','backend':'openrouter','prompt':prompt}
        return api({'op':'start','spec':spec})
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
        assert api({'op':'runtime_info'})['backends'][1]['configured']
        log=directory/'runtime-log.jsonl';api({'op':'log_file','path':str(log),'filter':{'targets':['runtime'],'events':['inference.']}})
        ordinary=start('WAIT_EFFECT');early=until(ordinary,'assistant_delta');assert not early['done']
        events,done=collect(ordinary);assert done['status']=='completed',done
        assert result(ordinary)['response']=='ORDINARY_RESPONSE'
        assert (directory/'effect.txt').read_text()=='once'
        replay,_=collect(ordinary,events[len(events)//2]['seq']);assert [e['seq'] for e in replay]==[e['seq'] for e in events if e['seq']>events[len(events)//2]['seq']]
        follow=start('FOLLOWUP',ordinary['record']['spec']['thread_id']);assert collect(follow)[1]['status']=='completed'
        assert result(follow)['response']=='FOLLOWUP_RESPONSE'
        print('PASS remote shell effect, realtime observation, reconnect replay, conventional response and native metadata in follow-up')
        truncated=start('TRUNCATED');assert collect(truncated)[1]['status']=='interrupted';assert not (directory/'forbidden.txt').exists()
        bad=start('BAD_ARGUMENTS');assert collect(bad)[1]['status']=='completed';assert result(bad)['response']=='ARGUMENT_REJECTION_OBSERVED'
        interrupted=start('INTERRUPT_AFTER_EFFECT');assert collect(interrupted)[1]['status']=='interrupted'
        assert (directory/'uncertain.txt').read_text()=='once'
        # Restart the executing daemon. No prior prompt or tool is automatically submitted again.
        processes[1].kill();processes[1].wait(timeout=5);tokens[1]=daemon(1)
        recovery=start('RECOVER',interrupted['record']['spec']['thread_id']);assert collect(recovery)[1]['status']=='completed'
        assert (directory/'uncertain.txt').read_text()=='once'
        print('PASS no truncated-call execution, malformed argument rejection, restart continuation and no completed-effect replay')
        for prompt,wait_kind in [('SLOW_HTTP','inference_attempt'),('SLOW_SHELL','tool_started')]:
            run=start(prompt);until(run,wait_kind);begin=time.monotonic();api({'op':'control','id':run['record']['spec']['id'],'action':{'type':'cancel'}})
            assert collect(run)[1]['status']=='cancelled';assert time.monotonic()-begin<3
            if prompt=='SLOW_SHELL':
                recovered=start('RECOVER',run['record']['spec']['thread_id']);recovery_events,done=collect(recovered);assert done['status']=='completed'
                assert any(e['kind']=='context_projection' and e['data']['unknown_tool_results']==1 for e in recovery_events)
        steer=start('STEER_ME');until(steer,'tool_started');api({'op':'control','id':steer['record']['spec']['id'],'action':{'type':'steer','text':'STEERING_TEXT'}})
        assert collect(steer)[1]['status']=='completed';assert result(steer)['response']=='STEERED_RESPONSE'
        print('PASS cancellation during HTTP/tool execution and steering at the next inference boundary')
        r=resident();m1=send(r,'SILENT one');m2=send(r,'REPLY one');m3=send(r,'SILENT two')
        activation=activate(r);assert len(activation['record']['mailbox']['message_ids'])==3
        assert collect(activation)[1]['status']=='completed'
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
