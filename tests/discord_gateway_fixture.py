"""Tiny RFC6455/Discord test peer; no external service or credentials."""
import base64
import hashlib
import json
import select
import socket
import socketserver
import struct
import threading

class GatewayFixture:
    def __init__(self):
        self.lock=threading.Lock();self.history=[];self.sequence=0;self.auth=[];self.presences=[];self.connections=[];self.session='fixture-session-one';self.ack=True;self.invalidate=False
        fixture=self
        class Peer(socketserver.BaseRequestHandler):
            def handle(self):
                sock=self.request;sock.settimeout(3)
                def exact(n):
                    out=b''
                    while len(out)<n:
                        b=sock.recv(n-len(out))
                        if not b:raise EOFError()
                        out+=b
                    return out
                def send(value):
                    b=json.dumps(value).encode();head=bytes([129,len(b)]) if len(b)<126 else bytes([129,126])+struct.pack('!H',len(b));sock.sendall(head+b)
                def read():
                    head=exact(2);n=head[1]&127
                    if n==126:n=struct.unpack('!H',exact(2))[0]
                    elif n==127:n=struct.unpack('!Q',exact(8))[0]
                    assert n<65536
                    mask=exact(4) if head[1]&128 else b'';b=exact(n)
                    if mask:b=bytes(v^mask[i%4] for i,v in enumerate(b))
                    if head[0]&15==8:raise EOFError()
                    return json.loads(b)
                try:
                    raw=b''
                    while b'\r\n\r\n' not in raw:raw+=exact(1)
                    assert raw.split(b'\r\n',1)[0].split()[1].startswith(b'/'), 'HTTP upgrade target needs a path'
                    headers=dict(line.split(':',1) for line in raw.decode().split('\r\n')[1:] if ':' in line)
                    key=next(v.strip() for k,v in headers.items() if k.lower()=='sec-websocket-key')
                    accept=base64.b64encode(hashlib.sha1((key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
                    sock.sendall(('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: '+accept+'\r\n\r\n').encode())
                    send({'op':10,'d':{'heartbeat_interval':500}});auth=read();assert auth['d']['token']=='fixture-a'
                    with fixture.lock:
                        fixture.auth.append({'op':auth['op'],'seq':auth['d'].get('seq')});fixture.connections.append(sock)
                    if auth['op']==6 and fixture.invalidate:
                        fixture.invalidate=False;fixture.session='fixture-session-two';fixture.sequence=0;fixture.history=[];send({'op':9,'d':False});return
                    if auth['op']==2:
                        fixture.push('READY',{'session_id':fixture.session,'resume_gateway_url':fixture.url,'user':{'id':'1001','bot':True}})
                        fixture.push('GUILD_CREATE',{'id':'space-one','channels':[{'id':'999','guild_id':'space-one','type':0}]})
                        cursor=0
                    else:
                        cursor=auth['d']['seq'];send({'op':0,'t':'RESUMED','s':cursor,'d':{}})
                    while True:
                        with fixture.lock:pending=[e for e in fixture.history if e['s']>cursor]
                        for event in pending:send(event);cursor=event['s']
                        if select.select([sock],[],[],.05)[0]:
                            value=read()
                            if value['op']==1 and fixture.ack:send({'op':11,'d':None})
                            elif value['op']==3:fixture.presences.append(value['d'])
                except (OSError,EOFError,ValueError):pass
                finally:
                    with fixture.lock:
                        if sock in fixture.connections:fixture.connections.remove(sock)
        class Server(socketserver.ThreadingTCPServer):
            allow_reuse_address=True;daemon_threads=True
        self.server=Server(('127.0.0.1',0),Peer);self.url='ws://127.0.0.1:'+str(self.server.server_address[1]);threading.Thread(target=self.server.serve_forever,daemon=True).start()
    def push(self,kind,data):
        with self.lock:
            self.sequence+=1;e={'op':0,'t':kind,'s':self.sequence,'d':data};self.history.append(e);return self.sequence
    def disconnect(self):
        with self.lock:sockets=list(self.connections)
        for sock in sockets:
            try:sock.shutdown(socket.SHUT_RDWR)
            except OSError:pass
    def close(self):self.disconnect();self.server.shutdown();self.server.server_close()
