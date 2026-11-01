#!/usr/bin/env python3
"""Paired daemon native-media verification. --live spends real OpenRouter credit.
Live fixtures need Pillow, ffmpeg and macOS say; daemon has no such dependencies.
"""
import argparse, base64, hashlib, http.server, json, pathlib, secrets, socket
import subprocess, tempfile, threading, urllib.request, shutil

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary',nargs='?',default='target/release/boxen')
parser.add_argument('--live',action='store_true')
parser.add_argument('--report',type=pathlib.Path)
parser.add_argument('--case',action='append',nargs=2,metavar=('MODEL','MODALITY'))
parser.add_argument('--model',action='append',help='Override live model matrix with an all-modality model')
args=parser.parse_args()
binary=pathlib.Path(args.binary).resolve();command=(['/bin/bash'] if binary.suffix=='.com' else [])+[str(binary)]
observed=[]
def frame(delta=None,finish=None):return {'choices':[{'index':0,'delta':delta or {},'finish_reason':finish}]}
class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self,*_):pass
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers['Content-Length'])));observed.append(body)
        assert 'Authorization' not in self.headers
        messages=body['messages'];prompt=next(m['content'] for m in reversed(messages) if m['role']=='user' and isinstance(m['content'],str))
        self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
        def emit(f):self.wfile.write(('data: '+json.dumps(f)+'\n\n').encode());self.wfile.flush()
        if not any(m['role']=='tool' for m in messages):
            paths=json.loads(prompt[6:]) if prompt.startswith('BATCH:') else [prompt]
            emit(frame({'tool_calls':[{'index':i,'id':f'read-{i}','type':'function','function':{'name':'boxen_read','arguments':json.dumps({'path':path})}} for i,path in enumerate(paths)]},'tool_calls'))
        else:
            emit(frame({'content':'observed'},'stop'))
        self.wfile.write(b'data: [DONE]\n\n')
provider=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider)
threading.Thread(target=provider.serve_forever,daemon=True).start()
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
def pdf(text):
    stream=f'BT /F1 24 Tf 40 140 Td ({text}) Tj ET'.encode()
    objects=[b'<< /Type /Catalog /Pages 2 0 R >>',b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>',b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',b'<< /Length '+str(len(stream)).encode()+b' >>\nstream\n'+stream+b'\nendstream']
    out=b'%PDF-1.4\n';offsets=[0]
    for i,o in enumerate(objects,1):offsets.append(len(out));out+=f'{i} 0 obj\n'.encode()+o+b'\nendobj\n'
    xref=len(out);out+=b'xref\n0 6\n0000000000 65535 f \n'+b''.join(f'{o:010} 00000 n \n'.encode() for o in offsets[1:]);return out+f'trailer << /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode()
report=[]
with tempfile.TemporaryDirectory(prefix='boxen-media-') as temp:
    root=pathlib.Path(temp);data=[root/'a',root/'b'];peers=[port(),port()];uis=[port(),port()];processes=[];tokens=[]
    def cli(i,*a):return subprocess.check_output(command+['--data-dir',str(data[i]),*a],text=True,timeout=10).strip()
    keys=[cli(i,'identity') for i in range(2)]
    for i in range(2):cli(i,'trust',keys[1-i],'Other')
    def daemon(i):
        options=['--openrouter-key-file',str(pathlib.Path.home()/'.boxen/credentials/openrouter.key')] if args.live else ['--openrouter-endpoint',f'http://127.0.0.1:{provider.server_port}/chat']
        p=subprocess.Popen(command+['--data-dir',str(data[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}',*options],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True);processes.append(p)
        lines=[p.stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1],lines;return lines[-1].strip().split('#')[1]
    def api(op,watch=False):
        r=urllib.request.Request(f'http://127.0.0.1:{uis[0]}/api/'+('watch' if watch else 'operation'),data=json.dumps({'peer':keys[1],'address':f'127.0.0.1:{peers[1]}','request':op}).encode(),headers={'Authorization':'Bearer '+tokens[0],'Content-Type':'application/json'})
        response=urllib.request.urlopen(r,timeout=160);return response if watch else json.load(response)
    def execute(prompt,model='fixture',thread=None):
        ident=secrets.token_hex(32);run=api({'op':'start','spec':{'id':ident,'thread_id':thread or ident,'backend':'openrouter','cwd':temp,'model':model,'prompt':prompt}});events=[]
        with api({'id':ident,'after':0},True) as stream:
            for line in stream:
                batch=json.loads(line);assert 'error' not in batch,batch;events+=batch['events']
                if batch['done']:return ident,events,batch['snapshot']
        raise AssertionError('watch ended early')
    def replay(ident):
        events=[]
        with api({'id':ident,'after':0},True) as stream:
            for line in stream:
                batch=json.loads(line);events+=batch['events']
                if batch['done']:return events
    try:
        tokens=[daemon(i) for i in range(2)]
        marker='AMBER '+secrets.token_hex(3).upper()
        (root/'document.pdf').write_bytes(pdf(marker));(root/'text.txt').write_text(marker)
        if args.live:
            from PIL import Image, ImageDraw
            color=secrets.choice(['red','blue','green']);count=secrets.choice([2,3,4]);shape=secrets.choice(['circle','square'])
            im=Image.new('RGB',(320,160),'white');draw=ImageDraw.Draw(im)
            for i in range(count):
                box=(10+i*75,40,60+i*75,90)
                (draw.ellipse if shape=='circle' else draw.rectangle)(box,fill=color)
            image_name=secrets.token_hex(8)+'.png';im.save(root/image_name)
            phrase=secrets.choice(['purple bicycle','silver umbrella','orange telescope','yellow kangaroo'])
            audio_name=secrets.token_hex(8)+'.wav'
            subprocess.run(['say','-v','Samantha','-o',str(root/'source.aiff'),'The secret phrase is '+phrase+'.'],check=True)
            subprocess.run(['ffmpeg','-v','error','-i',str(root/'source.aiff'),'-ar','16000','-ac','1',str(root/audio_name)],check=True)
            first,second=secrets.SystemRandom().sample(['red','blue','green','yellow'],2)
            video_name=secrets.token_hex(8)+'.mp4'
            subprocess.run(['ffmpeg','-v','error','-f','lavfi','-i',f'color=c={first}:s=160x120:d=2:r=2','-f','lavfi','-i',f'color=c={second}:s=160x120:d=2:r=2','-filter_complex','[0:v][1:v]concat=n=2:v=1:a=0[v]','-map','[v]','-c:v','libx264','-pix_fmt','yuv420p',str(root/video_name)],check=True)
            pdf_name=secrets.token_hex(8)+'.pdf';(root/'document.pdf').rename(root/pdf_name)
            text_name=secrets.token_hex(8)+'.txt';(root/'text.txt').rename(root/text_name)
            cases={'text':(text_name,'What exact text is in the file?',lambda s:marker.lower() in s.lower()),'image':(image_name,'How many shapes are shown, and what color and shape are they?',lambda s:color in s.lower() and (str(count) in s or {2:'two',3:'three',4:'four'}[count] in s.lower()) and shape in s.lower()),'audio':(audio_name,'What secret phrase is spoken?',lambda s:phrase in s.lower()),'video':(video_name,'What color appears first and what color appears second?',lambda s:first in s.lower() and second in s.lower() and s.lower().index(first)<s.lower().index(second)),'file':(pdf_name,'What exact text is on the page?',lambda s:marker.lower() in s.lower())}
            matrix={m:list(cases) for m in args.model} if args.model else {'google/gemini-2.5-flash-lite':list(cases),'xiaomi/mimo-v2.6-flash':['image','audio','video'],'qwen/qwen3.8-omni-flash':['audio','video'],'openai/gpt-4.1-nano':['image','file'],'deepseek/deepseek-v4.1-flash':['image']}
            if args.case:
                matrix={}
                for model,kind in args.case: matrix.setdefault(model,[]).append(kind)
            expected={'text':marker,'image':f'{count} {color} {shape}s','audio':phrase,'video':[first,second],'file':marker}
            if args.report:
                fixtures=args.report.parent/(args.report.stem+'-fixtures');fixtures.mkdir(parents=True,exist_ok=True)
                for filename,_,_ in cases.values():shutil.copyfile(root/filename,fixtures/filename)
            for model,kinds in matrix.items():
                for kind in kinds:
                    filename,question,check=cases[kind]
                    ident,events,snapshot=execute(f'Please call boxen_read on {filename}, then answer using the returned file. {question} No shell commands or other file access are requested.',model)
                    reads=[e for e in events if e['kind']=='tool_result' and e['data']['tool']=='boxen_read']
                    response=next((e['data']['text'] for e in reversed(events) if e['kind']=='assistant_message'),'')
                    usage=[e['data']['usage'] for e in events if e['kind']=='inference_exchange_completed']
                    row={'model':model,'modality':kind,'expected':expected[kind],'file':filename,'sha256':hashlib.sha256((root/filename).read_bytes()).hexdigest(),'bytes':(root/filename).stat().st_size,'status':snapshot['status'],'read':any(e['data']['result'].get('ok') is True for e in reads),'tools':[e['data']['tool'] for e in events if e['kind']=='tool_started'],'perceived':check(response),'response':response,'cost':sum(u.get('cost',0) for u in usage),'detail':snapshot.get('detail') or next((e['data'].get('detail') for e in reversed(events) if e['kind']=='status'),None)}
                    report.append(row);print(json.dumps(row),flush=True)
                    assert replay(ident)==events
            assert all(r['status']=='completed' and r['read'] and r['perceived'] and all(t=='boxen_read' for t in r['tools']) for r in report), 'live matrix has failures; report preserves evidence'
        else:
            for filename,content,part in [('picture.png',b'\x89PNG\r\n\x1a\nfixture','image_url'),('sound.wav',b'RIFF1234WAVEfixture','input_audio'),('clip.mp4',b'0000ftypisomfixture','video_url'),('document.pdf',pdf(marker),'file'),('text.txt',marker.encode(),None)]:
                (root/filename).write_bytes(content);ident,events,snapshot=execute(filename);assert snapshot['status']=='completed',snapshot
                result=next(e['data']['result'] for e in events if e['kind']=='tool_result')
                assert result['sha256']==hashlib.sha256(content).hexdigest()
                if part:
                    wire=observed[-1];parts=wire['messages'][-1]['content'];native=next(p for p in parts if p['type']==part)
                    if part=='input_audio':encoded=native[part]['data']
                    else:encoded=(native[part]['file_data'] if part=='file' else native[part]['url']).split(',',1)[1]
                    assert base64.b64decode(encoded)==content
                    if part=='file':assert wire['plugins']==[{'id':'file-parser','pdf':{'engine':'native'}}]
                    assert any('tool output' in p.get('text','') for p in parts)
                    source=root/filename;source.unlink()
                    # Restart executor and prove projected input comes from retained snapshots.
                    processes[-1].kill();processes[-1].wait();tokens[1]=daemon(1)
                    assert replay(ident)==events
                    _,_,follow=execute('FOLLOWUP',thread=ident);assert follow['status']=='completed'
                    assert any(p.get('type')==part for m in observed[-1]['messages'] if isinstance(m['content'],list) for p in m['content'])
                    snapshot_file=data[1]/'runs'/ident/'artifacts'/result['sha256'];assert snapshot_file.stat().st_mode & 0o777 == 0o600; snapshot_file.write_bytes(b'corrupt')
                    _,bad,follow=execute('FOLLOWUP',thread=ident);assert follow['status']=='completed'
                    assert any(e['kind']=='artifact_unavailable' for e in bad)
                assert replay(ident)==events
            # Nine distinct 2 MiB snapshots hit the enforced 16 MiB run quota.
            paths=[]
            for i in range(9):
                name=f'quota-{i}.wav';paths.append(name)
                (root/name).write_bytes(b'RIFF1234WAVE'+bytes([i])*(2*1024*1024-12))
            _,events,snapshot=execute('BATCH:'+json.dumps(paths));assert snapshot['status']=='completed'
            results=[e['data']['result'] for e in events if e['kind']=='tool_result']
            assert len(results)==9 and sum(r['ok'] for r in results)==8,results
            assert 'quota' in results[-1]['error']
            assert len([p for p in observed[-1]['messages'][-1]['content'] if p['type']=='input_audio'])==8
            assert all('boxen_artifact' not in m for m in observed[-1]['messages'])
            for name,content in [('unknown.bin' ,b'\x00\xff'),('large.bin',b'x'*(2*1024*1024+1))]:
                (root/name).write_bytes(content);_,events,_=execute(name)
                assert next(e['data']['result'] for e in events if e['kind']=='tool_result')['ok']==False
            print(json.dumps({'ok':True,'native_parts':4,'text':True,'restart_replay':True,'source_independence':True,'corruption_recovery':True,'unknown_and_oversize_rejected':True,'run_quota_and_22MiB_request':True}))
    finally:
        if args.report:args.report.parent.mkdir(parents=True,exist_ok=True);args.report.write_text(json.dumps(report,indent=2)+'\n')
        for p in processes:
            if p.poll() is None:p.kill()
            p.wait(timeout=5)
        provider.shutdown()
